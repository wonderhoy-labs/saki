//! Turns a client's IP packets into ordinary sockets on the server.
//!
//! Every client gets its own userspace TCP/IP stack. Its TCP connections and UDP flows end here,
//! and matching sockets carry them onward, so the server needs no root, kernel forwarding or
//! firewall rules. Those sockets can be tied to one network device, which is how a client's
//! traffic ends up going through something like an `xray0` TUN.

use crate::checksum;
use crate::proto::{GATEWAY_V4, GATEWAY_V6, MTU, PREFIX_V6};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use netstack_smoltcp::udp::{ReadHalf, UdpMsg, WriteHalf};
use netstack_smoltcp::{StackBuilder, TcpListener, TcpStream as StackTcpStream};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpSocket, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, trace, warn};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const ECHO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING_ECHOES: usize = 64;
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const UDP_FLOW_QUEUE: usize = 32;

/// Packets waiting to go out over the link. Bluetooth is slow enough that a longer queue only adds
/// delay. When this fills, the stack holds back rather than dropping, and TCP paces itself to the
/// radio. UDP has no such signal, so its datagrams are dropped instead.
pub const LINK_QUEUE_PACKETS: usize = 32;
const INPUT_QUEUE_PACKETS: usize = 256;

/// The TCP window each way, per connection. A Bluetooth link can't use more than this.
const TCP_BUFFER_BYTES: u32 = 32 * 1024;

/// How a server treats its clients' traffic.
#[derive(Clone, Debug)]
pub struct NatConfig {
	/// Send everything through this network device instead of following the routing table.
	pub device: Option<String>,
	/// Where DNS over TCP to the tunnel's gateway address goes.
	pub upstream_dns: Ipv4Addr,
	/// Refuse private addresses, so that clients can't reach the server's local network.
	pub block_lan: bool,
	/// Allow the server's own loopback addresses. Only the tests want this.
	pub allow_loopback: bool,
}

impl NatConfig {
	pub fn new(upstream_dns: Ipv4Addr) -> Self {
		NatConfig { device: None, upstream_dns, block_lan: false, allow_loopback: false }
	}

	/// Checks that the configured device exists and can be used, so that a typo or a missing
	/// permission fails at startup rather than on the client's first connection.
	pub fn check_device(&self) -> io::Result<()> {
		let Some(device) = &self.device else {
			return Ok(());
		};
		let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
		bind_to_device(&socket, device, false)
			.map_err(|error| io::Error::new(error.kind(), format!("can't use network device {device}: {error}")))
	}

	/// Where a connection to `target` should really go, or `None` if it isn't allowed.
	fn route(&self, target: SocketAddr) -> Option<SocketAddr> {
		let ip = target.ip().to_canonical();
		if in_tunnel(ip) {
			return (ip == IpAddr::V4(GATEWAY_V4) && target.port() == 53)
				.then(|| SocketAddr::new(IpAddr::V4(self.upstream_dns), 53));
		}
		self.allowed(ip).then_some(target)
	}

	fn allowed(&self, ip: IpAddr) -> bool {
		if ip.is_loopback() {
			return self.allow_loopback;
		}
		let is_lan = match ip {
			IpAddr::V4(ip) => {
				if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() || ip.is_link_local() {
					return false;
				}
				let is_cgnat = ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64;
				ip.is_private() || is_cgnat
			}
			IpAddr::V6(ip) => {
				let is_link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
				if ip.is_unspecified() || ip.is_multicast() || is_link_local {
					return false;
				}
				ip.segments()[0] & 0xfe00 == 0xfc00
			}
		};
		!(is_lan && self.block_lan)
	}
}

/// True for addresses that only mean something inside the tunnel.
fn in_tunnel(ip: IpAddr) -> bool {
	match ip {
		IpAddr::V4(ip) => {
			let [a, b, c, _] = ip.octets();
			let [ga, gb, gc, _] = GATEWAY_V4.octets();
			(a, b, c) == (ga, gb, gc)
		}
		IpAddr::V6(ip) => {
			let mask = u128::MAX << (128 - u32::from(PREFIX_V6));
			u128::from(ip) & mask == u128::from(GATEWAY_V6) & mask
		}
	}
}

/// One client's stack. Dropping it ends the client's connections.
pub struct Nat {
	input: mpsc::Sender<Bytes>,
	output: Option<mpsc::Receiver<Bytes>>,
	supervisor: JoinHandle<()>,
}

impl Nat {
	/// Starts a stack on the current Tokio runtime.
	pub fn start(config: Arc<NatConfig>) -> Self {
		let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE_PACKETS);
		let (output_tx, output_rx) = mpsc::channel(LINK_QUEUE_PACKETS);
		let supervisor = tokio::spawn(run_stack(config, input_rx, output_tx));
		Nat { input: input_tx, output: Some(output_rx), supervisor }
	}

	/// A handle for feeding in packets from the client.
	pub fn input(&self) -> NatInput {
		NatInput(self.input.clone())
	}

	/// Packets for the client. There's only one receiver, so this can only be taken once.
	pub fn take_output(&mut self) -> Option<mpsc::Receiver<Bytes>> {
		self.output.take()
	}
}

impl Drop for Nat {
	fn drop(&mut self) {
		self.supervisor.abort();
	}
}

#[derive(Clone)]
pub struct NatInput(mpsc::Sender<Bytes>);

impl NatInput {
	/// Hands the stack a packet from the client, dropping it if the stack has fallen behind.
	pub fn send(&self, packet: Bytes) {
		if self.0.try_send(packet).is_err() {
			trace!("nat input queue full, dropping a packet");
		}
	}
}

/// Runs everything belonging to one stack. Aborting this drops the JoinSet, which stops the rest.
async fn run_stack(config: Arc<NatConfig>, mut input: mpsc::Receiver<Bytes>, output: mpsc::Sender<Bytes>) {
	let built = StackBuilder::default()
		.enable_tcp(true)
		.enable_udp(true)
		.mtu(usize::from(MTU))
		.stack_buffer_size(LINK_QUEUE_PACKETS)
		.tcp_recv_buffer_size(TCP_BUFFER_BYTES)
		.tcp_send_buffer_size(TCP_BUFFER_BYTES)
		.build();
	let (stack, runner, udp, tcp) = match built {
		Ok(built) => built,
		Err(error) => {
			warn!(%error, "couldn't start a network stack");
			return;
		}
	};
	let (Some(runner), Some(udp), Some(tcp)) = (runner, udp, tcp) else {
		warn!("the network stack came up without TCP or UDP");
		return;
	};
	let (mut stack_sink, mut stack_stream) = stack.split();
	let (udp_read, udp_write) = udp.split();

	let mut tasks = JoinSet::new();
	tasks.spawn(async move {
		if let Err(error) = runner.await {
			debug!(%error, "the network stack stopped");
		}
	});
	let flows = TcpFlows::default();
	tasks.spawn(accept_tcp(tcp, flows.clone()));
	tasks.spawn(relay_all_udp(udp_read, udp_write, config.clone()));

	let stack_output = output.clone();
	tasks.spawn(async move {
		// Waiting here is what lets a full link hold the TCP stack back.
		while let Some(Ok(packet)) = stack_stream.next().await {
			if stack_output.send(Bytes::from(packet)).await.is_err() {
				break;
			}
		}
	});

	let (to_stack, mut stack_queue) = mpsc::channel::<Vec<u8>>(INPUT_QUEUE_PACKETS);
	tasks.spawn(async move {
		while let Some(packet) = stack_queue.recv().await {
			if stack_sink.send(packet).await.is_err() {
				break;
			}
		}
	});

	let echo_slots = Arc::new(Semaphore::new(MAX_PENDING_ECHOES));
	while let Some(packet) = input.recv().await {
		if packet.len() > usize::from(MTU) {
			continue;
		}
		if let Some(echo) = EchoRequest::parse(&packet) {
			handle_echo(echo, &config, &echo_slots, &output, &mut tasks);
			continue;
		}
		if let Some(syn) = TcpSyn::parse(&packet) {
			match flows.on_syn(syn.key()) {
				SynAction::Connect => {
					let context = ConnectContext {
						syn,
						packet: packet.to_vec(),
						config: config.clone(),
						flows: flows.clone(),
						to_stack: to_stack.clone(),
						output: output.clone(),
					};
					tasks.spawn(connect_flow(context));
					continue;
				}
				SynAction::Ignore => continue,
				SynAction::Forward => {}
			}
		}
		if to_stack.send(packet.to_vec()).await.is_err() {
			break;
		}
	}
}

type FlowKey = (SocketAddr, SocketAddr);

enum TcpFlow {
	Connecting,
	/// The real connection is up, and the stack is about to hand over its half.
	Ready(TcpStream),
	Relaying,
}

enum SynAction {
	/// A new connection. Reach the destination first.
	Connect,
	/// A repeat of a SYN still waiting on its destination.
	Ignore,
	/// A repeat of a SYN that's already gone through. The stack should see it, in case its answer
	/// got lost.
	Forward,
}

/// The TCP connections of one client. The stack answers a SYN as soon as it sees one, so it only
/// gets to see it once the real connection exists. That way a client finds out when a destination
/// is unreachable, instead of connecting and then being dropped, and things like IPv4 fallback on a
/// server with no IPv6 keep working.
#[derive(Clone, Default)]
struct TcpFlows(Arc<Mutex<HashMap<FlowKey, TcpFlow>>>);

impl TcpFlows {
	fn on_syn(&self, key: FlowKey) -> SynAction {
		let mut flows = self.0.lock().unwrap();
		match flows.get(&key) {
			None => {
				flows.insert(key, TcpFlow::Connecting);
				SynAction::Connect
			}
			Some(TcpFlow::Connecting) => SynAction::Ignore,
			Some(TcpFlow::Ready(_) | TcpFlow::Relaying) => SynAction::Forward,
		}
	}

	fn mark_ready(&self, key: FlowKey, stream: TcpStream) {
		self.0.lock().unwrap().insert(key, TcpFlow::Ready(stream));
	}

	/// Takes the real connection for a stream the stack just produced. A repeated SYN makes the
	/// stack produce a spare stream, and there's nothing to take for that one.
	fn take_ready(&self, key: FlowKey) -> Option<TcpStream> {
		let mut flows = self.0.lock().unwrap();
		match flows.remove(&key) {
			Some(TcpFlow::Ready(stream)) => {
				flows.insert(key, TcpFlow::Relaying);
				Some(stream)
			}
			Some(flow) => {
				flows.insert(key, flow);
				None
			}
			None => None,
		}
	}

	fn remove(&self, key: FlowKey) {
		self.0.lock().unwrap().remove(&key);
	}
}

struct ConnectContext {
	syn: TcpSyn,
	packet: Vec<u8>,
	config: Arc<NatConfig>,
	flows: TcpFlows,
	to_stack: mpsc::Sender<Vec<u8>>,
	output: mpsc::Sender<Bytes>,
}

async fn connect_flow(context: ConnectContext) {
	let ConnectContext { syn, packet, config, flows, to_stack, output } = context;
	let key = syn.key();
	let result = match config.route(syn.dst) {
		Some(route) => connect_tcp(route, &config).await,
		None => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
	};
	match result {
		Ok(stream) => {
			debug!(target = %syn.dst, "TCP connected");
			flows.mark_ready(key, stream);
			if to_stack.send(packet).await.is_err() {
				flows.remove(key);
			}
		}
		Err(error) => {
			debug!(target = %syn.dst, %error, "TCP connection failed");
			flows.remove(key);
			let _ = output.send(Bytes::from(syn.reset())).await;
		}
	}
}

async fn accept_tcp(mut listener: TcpListener, flows: TcpFlows) {
	let mut relays: JoinSet<FlowKey> = JoinSet::new();
	loop {
		tokio::select! {
			accepted = listener.next() => match accepted {
				Some((stream, client, target)) => {
					let key = (client, target);
					if let Some(outer) = flows.take_ready(key) {
						relays.spawn(relay_tcp(stream, outer, key));
					}
				}
				None => return,
			},
			Some(Ok(key)) = relays.join_next() => flows.remove(key),
		}
	}
}

async fn relay_tcp(mut inner: StackTcpStream, mut outer: TcpStream, key: FlowKey) -> FlowKey {
	if let Err(error) = tokio::io::copy_bidirectional(&mut inner, &mut outer).await {
		debug!(target = %key.1, %error, "TCP relay ended");
	}
	let _ = outer.shutdown().await;
	key
}

async fn connect_tcp(target: SocketAddr, config: &NatConfig) -> io::Result<TcpStream> {
	let socket = Socket::new(Domain::for_address(target), Type::STREAM, Some(Protocol::TCP))?;
	socket.set_nonblocking(true)?;
	if let Some(device) = &config.device {
		bind_to_device(&socket, device, target.is_ipv6())?;
	}
	let socket = TcpSocket::from_std_stream(socket.into());
	let stream = timeout(CONNECT_TIMEOUT, socket.connect(target))
		.await
		.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
	stream.set_nodelay(true)?;
	Ok(stream)
}

/// The stack hands over every client datagram on one channel, so flows are told apart here.
async fn relay_all_udp(mut read: ReadHalf, mut write: WriteHalf, config: Arc<NatConfig>) {
	let (reply_tx, mut reply_rx) = mpsc::channel::<UdpMsg>(LINK_QUEUE_PACKETS);
	let mut flows: HashMap<FlowKey, mpsc::Sender<Vec<u8>>> = HashMap::new();
	let mut tasks: JoinSet<FlowKey> = JoinSet::new();
	loop {
		tokio::select! {
			message = read.next() => {
				let Some((payload, client, target)) = message else { return };
				let key = (client, target);
				let flow = match flows.entry(key) {
					Entry::Occupied(flow) => flow.into_mut(),
					Entry::Vacant(slot) => {
						let Some(route) = config.route(target) else {
							debug!(%target, "refused a UDP flow");
							continue;
						};
						let socket = match connect_udp(route, &config) {
							Ok(socket) => socket,
							Err(error) => {
								debug!(%route, %error, "couldn't open a UDP socket");
								continue;
							}
						};
						let (flow_tx, flow_rx) = mpsc::channel(UDP_FLOW_QUEUE);
						tasks.spawn(relay_udp_flow(key, socket, flow_rx, reply_tx.clone()));
						slot.insert(flow_tx)
					}
				};
				let _ = flow.try_send(payload);
			}
			Some(reply) = reply_rx.recv() => {
				if write.send(reply).await.is_err() {
					return;
				}
			}
			Some(Ok(key)) = tasks.join_next() => {
				flows.remove(&key);
			}
		}
	}
}

async fn relay_udp_flow(
	key: FlowKey,
	socket: UdpSocket,
	mut from_client: mpsc::Receiver<Vec<u8>>,
	to_client: mpsc::Sender<UdpMsg>,
) -> FlowKey {
	let (client, target) = key;
	let header_len = if target.is_ipv4() { 28 } else { 48 };
	let max_datagram = usize::from(MTU) - header_len;
	let mut buffer = vec![0; 65536];
	let mut deadline = Instant::now() + UDP_IDLE_TIMEOUT;
	loop {
		tokio::select! {
			datagram = from_client.recv() => {
				let Some(datagram) = datagram else { return key };
				// Errors like "connection refused" don't end a flow. They're just lost datagrams.
				let _ = socket.send(&datagram).await;
				deadline = Instant::now() + UDP_IDLE_TIMEOUT;
			}
			received = socket.recv(&mut buffer) => match received {
				// Bigger than the tunnel can carry, and this stack can't fragment it.
				Ok(len) if len > max_datagram => debug!(%target, len, "dropped an oversized datagram"),
				Ok(len) => {
					let reply = (buffer[..len].to_vec(), target, client);
					if to_client.try_send(reply).is_err() {
						trace!("UDP reply queue full, dropping a datagram");
					}
					deadline = Instant::now() + UDP_IDLE_TIMEOUT;
				}
				Err(error) => trace!(%target, %error, "UDP receive failed"),
			},
			() = sleep_until(deadline) => return key,
		}
	}
}

fn connect_udp(target: SocketAddr, config: &NatConfig) -> io::Result<UdpSocket> {
	let socket = Socket::new(Domain::for_address(target), Type::DGRAM, Some(Protocol::UDP))?;
	socket.set_nonblocking(true)?;
	if let Some(device) = &config.device {
		bind_to_device(&socket, device, target.is_ipv6())?;
	}
	socket.connect(&target.into()).or_else(ignore_in_progress)?;
	UdpSocket::from_std(socket.into())
}

fn ignore_in_progress(error: io::Error) -> io::Result<()> {
	if error.raw_os_error() == Some(libc::EINPROGRESS) { Ok(()) } else { Err(error) }
}

/// A client's attempt to open a TCP connection.
struct TcpSyn {
	src: SocketAddr,
	dst: SocketAddr,
	seq: u32,
}

impl TcpSyn {
	fn parse(packet: &[u8]) -> Option<Self> {
		let version = packet.first()? >> 4;
		let (src, dst, segment): (IpAddr, IpAddr, &[u8]) = if version == 4 {
			let header_len = usize::from(packet[0] & 0x0f) * 4;
			if packet.len() < 20 || packet[9] != 6 || header_len < 20 {
				return None;
			}
			let src: [u8; 4] = packet[12..16].try_into().ok()?;
			let dst: [u8; 4] = packet[16..20].try_into().ok()?;
			(IpAddr::from(src), IpAddr::from(dst), packet.get(header_len..)?)
		} else if version == 6 {
			if packet.len() < 40 || packet[6] != 6 {
				return None;
			}
			let src: [u8; 16] = packet[8..24].try_into().ok()?;
			let dst: [u8; 16] = packet[24..40].try_into().ok()?;
			(IpAddr::from(src), IpAddr::from(dst), &packet[40..])
		} else {
			return None;
		};
		if segment.len() < 20 || segment[13] & 0x12 != 0x02 {
			return None;
		}
		Some(TcpSyn {
			src: SocketAddr::new(src, u16::from_be_bytes([segment[0], segment[1]])),
			dst: SocketAddr::new(dst, u16::from_be_bytes([segment[2], segment[3]])),
			seq: u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]),
		})
	}

	fn key(&self) -> FlowKey {
		(self.src, self.dst)
	}

	/// Builds the reset that tells the client this connection isn't going to happen.
	fn reset(&self) -> Vec<u8> {
		let mut segment = [0; 20];
		segment[0..2].copy_from_slice(&self.dst.port().to_be_bytes());
		segment[2..4].copy_from_slice(&self.src.port().to_be_bytes());
		segment[8..12].copy_from_slice(&self.seq.wrapping_add(1).to_be_bytes());
		segment[12] = 5 << 4;
		segment[13] = 0x14; // RST, ACK
		match (self.dst.ip(), self.src.ip()) {
			(IpAddr::V4(from), IpAddr::V4(to)) => {
				let sum = checksum::tcp_v4(from.octets(), to.octets(), &segment);
				segment[16..18].copy_from_slice(&sum.to_be_bytes());
				ipv4_packet(from, to, 6, &segment)
			}
			(IpAddr::V6(from), IpAddr::V6(to)) => {
				let sum = checksum::tcp_v6(from, to, &segment);
				segment[16..18].copy_from_slice(&sum.to_be_bytes());
				ipv6_packet(from, to, 6, &segment)
			}
			_ => Vec::new(),
		}
	}
}

/// A ping from the client. Other ICMP messages have nothing to act on here and aren't parsed.
struct EchoRequest {
	src: IpAddr,
	dst: IpAddr,
	message: Vec<u8>,
}

impl EchoRequest {
	fn parse(packet: &[u8]) -> Option<Self> {
		let version = packet.first()? >> 4;
		let (src, dst, message): (IpAddr, IpAddr, &[u8]) = if version == 4 {
			let header_len = usize::from(packet[0] & 0x0f) * 4;
			let fragment = u16::from_be_bytes([*packet.get(6)?, *packet.get(7)?]);
			if *packet.get(9)? != 1 || fragment & 0x3fff != 0 || packet.len() < header_len || header_len < 20 {
				return None;
			}
			let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]])).min(packet.len());
			let src: [u8; 4] = packet[12..16].try_into().ok()?;
			let dst: [u8; 4] = packet[16..20].try_into().ok()?;
			(IpAddr::from(src), IpAddr::from(dst), packet.get(header_len..total_len)?)
		} else if version == 6 {
			if packet.len() < 40 || packet[6] != 58 {
				return None;
			}
			let src: [u8; 16] = packet[8..24].try_into().ok()?;
			let dst: [u8; 16] = packet[24..40].try_into().ok()?;
			(IpAddr::from(src), IpAddr::from(dst), &packet[40..])
		} else {
			return None;
		};
		let echo_type = if version == 4 { 8 } else { 128 };
		(message.len() >= 8 && message[0] == echo_type).then(|| EchoRequest { src, dst, message: message.to_vec() })
	}

	/// Builds the IP packet answering this ping, using `body` for its identifier-less contents.
	fn reply(&self, body: &[u8]) -> Option<Bytes> {
		if body.len() < 8 {
			return None;
		}
		let mut message = body.to_vec();
		message[1] = 0;
		message[2..4].fill(0);
		message[4..6].copy_from_slice(&self.message[4..6]);
		match (self.dst, self.src) {
			(IpAddr::V4(from), IpAddr::V4(to)) => {
				message[0] = 0;
				let sum = checksum::of(&message);
				message[2..4].copy_from_slice(&sum.to_be_bytes());
				Some(Bytes::from(ipv4_packet(from, to, 1, &message)))
			}
			(IpAddr::V6(from), IpAddr::V6(to)) => {
				message[0] = 129;
				let sum = checksum::icmpv6(from, to, &message);
				message[2..4].copy_from_slice(&sum.to_be_bytes());
				Some(Bytes::from(ipv6_packet(from, to, 58, &message)))
			}
			_ => None,
		}
	}
}

fn ipv4_packet(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
	let total_len = 20 + payload.len();
	let mut packet = vec![0; total_len];
	packet[0] = 0x45;
	packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
	packet[8] = 64;
	packet[9] = protocol;
	packet[12..16].copy_from_slice(&src.octets());
	packet[16..20].copy_from_slice(&dst.octets());
	let sum = checksum::of(&packet[..20]);
	packet[10..12].copy_from_slice(&sum.to_be_bytes());
	packet[20..].copy_from_slice(payload);
	packet
}

fn ipv6_packet(src: Ipv6Addr, dst: Ipv6Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
	let mut packet = vec![0; 40 + payload.len()];
	packet[0] = 0x60;
	packet[4..6].copy_from_slice(&(payload.len() as u16).to_be_bytes());
	packet[6] = protocol;
	packet[7] = 64;
	packet[8..24].copy_from_slice(&src.octets());
	packet[24..40].copy_from_slice(&dst.octets());
	packet[40..].copy_from_slice(payload);
	packet
}

fn handle_echo(
	echo: EchoRequest,
	config: &Arc<NatConfig>,
	slots: &Arc<Semaphore>,
	output: &mpsc::Sender<Bytes>,
	tasks: &mut JoinSet<()>,
) {
	let target = echo.dst.to_canonical();
	if in_tunnel(target) {
		// The tunnel's own address answers directly, which makes pinging it a test of the link.
		if (target == IpAddr::V4(GATEWAY_V4) || target == IpAddr::V6(GATEWAY_V6))
			&& let Some(reply) = echo.reply(&echo.message)
		{
			let _ = output.try_send(reply);
		}
		return;
	}
	if !config.allowed(target) {
		return;
	}
	let Ok(slot) = slots.clone().try_acquire_owned() else {
		return;
	};
	let config = config.clone();
	let output = output.clone();
	tasks.spawn(async move {
		let _slot = slot;
		match relay_echo(&echo, &config).await {
			Ok(reply) => {
				if let Some(reply) = echo.reply(&reply) {
					let _ = output.try_send(reply);
				}
			}
			Err(error) => debug!(%target, %error, "ping failed"),
		}
	});
}

/// Sends a ping using an unprivileged ICMP socket, and returns the reply's ICMP message. Systems
/// that don't allow those sockets just lose the ping.
async fn relay_echo(echo: &EchoRequest, config: &NatConfig) -> io::Result<Vec<u8>> {
	let target = SocketAddr::new(echo.dst, 0);
	let protocol = if target.is_ipv4() { Protocol::ICMPV4 } else { Protocol::ICMPV6 };
	let socket = Socket::new(Domain::for_address(target), Type::DGRAM, Some(protocol))?;
	socket.set_nonblocking(true)?;
	if let Some(device) = &config.device {
		bind_to_device(&socket, device, target.is_ipv6())?;
	}
	socket.connect(&target.into()).or_else(ignore_in_progress)?;
	let socket = UdpSocket::from_std(socket.into())?;
	socket.send(&echo.message).await?;

	let expected_type = if target.is_ipv4() { 0 } else { 129 };
	let mut buffer = vec![0; 65536];
	let wait = async {
		loop {
			let len = socket.recv(&mut buffer).await?;
			let mut message = &buffer[..len];
			// macOS includes the IPv4 header on these sockets. ICMP has no type 0x45.
			if message.first().is_some_and(|first| first >> 4 == 4) {
				let header_len = usize::from(message[0] & 0x0f) * 4;
				message = message.get(header_len..).unwrap_or_default();
			}
			// Linux rewrites the identifier, so match on the sequence number and the contents.
			if message.len() == echo.message.len() && message[0] == expected_type && message[6..] == echo.message[6..] {
				return io::Result::Ok(message.to_vec());
			}
		}
	};
	timeout(ECHO_TIMEOUT, wait).await.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

/// Ties a socket to a network device, so its traffic ignores the routing table.
fn bind_to_device(socket: &Socket, device: &str, ipv6: bool) -> io::Result<()> {
	#[cfg(any(target_os = "linux", target_os = "android"))]
	{
		let _ = ipv6;
		socket.bind_device(Some(device.as_bytes()))
	}
	#[cfg(target_os = "macos")]
	{
		let index = std::ffi::CString::new(device)
			.map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
			.map(|name| unsafe { libc::if_nametoindex(name.as_ptr()) })?;
		let index = std::num::NonZeroU32::new(index)
			.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such network device"))?;
		if ipv6 { socket.bind_device_by_index_v6(Some(index)) } else { socket.bind_device_by_index_v4(Some(index)) }
	}
	#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
	{
		let _ = (socket, device, ipv6);
		Err(io::Error::new(io::ErrorKind::Unsupported, "this platform can't bind to a network device"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn config() -> NatConfig {
		NatConfig::new(Ipv4Addr::new(1, 1, 1, 1))
	}

	fn target(address: &str) -> SocketAddr {
		address.parse().unwrap()
	}

	#[test]
	fn public_addresses_are_allowed() {
		assert_eq!(config().route(target("93.184.216.34:443")), Some(target("93.184.216.34:443")));
		assert_eq!(config().route(target("[2606:4700::1111]:443")), Some(target("[2606:4700::1111]:443")));
	}

	#[test]
	fn the_server_itself_is_off_limits() {
		for address in ["127.0.0.1:22", "[::1]:22", "[::ffff:127.0.0.1]:22", "0.0.0.0:22", "[::]:22"] {
			assert_eq!(config().route(target(address)), None, "{address}");
		}
	}

	#[test]
	fn non_unicast_and_link_local_are_refused() {
		for address in ["224.0.0.1:5353", "255.255.255.255:67", "169.254.169.254:80", "[fe80::1]:80", "[ff02::1]:80"] {
			assert_eq!(config().route(target(address)), None, "{address}");
		}
	}

	#[test]
	fn lan_addresses_follow_the_setting() {
		let lan = ["192.168.1.1:80", "10.0.0.1:80", "172.16.0.1:80", "100.64.0.1:80", "[fd00::1]:80"];
		for address in lan {
			assert_eq!(config().route(target(address)), Some(target(address)), "{address}");
		}
		let blocking = NatConfig { block_lan: true, ..config() };
		for address in lan {
			assert_eq!(blocking.route(target(address)), None, "{address}");
		}
	}

	#[test]
	fn dns_to_the_gateway_goes_upstream_and_nothing_else_does() {
		assert_eq!(config().route(target("10.233.77.1:53")), Some(target("1.1.1.1:53")));
		assert_eq!(config().route(target("10.233.77.1:80")), None);
		assert_eq!(config().route(target("10.233.77.9:53")), None);
		assert_eq!(config().route(target("[fd53:616b:6900::1]:53")), None);
	}

	#[test]
	fn echo_requests_are_recognized_and_answered() {
		let client = Ipv4Addr::new(10, 233, 77, 2);
		let mut request = vec![8, 0, 0, 0, 0x12, 0x34, 0, 7, 1, 2, 3];
		let sum = checksum::of(&request);
		request[2..4].copy_from_slice(&sum.to_be_bytes());
		let packet = ipv4_packet(client, GATEWAY_V4, 1, &request);

		let echo = EchoRequest::parse(&packet).unwrap();
		assert_eq!(echo.src, IpAddr::V4(client));
		let reply = echo.reply(&echo.message).unwrap();
		assert_eq!(&reply[12..16], &GATEWAY_V4.octets());
		assert_eq!(&reply[16..20], &client.octets());
		assert_eq!(checksum::of(&reply[..20]), 0);
		assert_eq!(reply[20], 0);
		assert_eq!(reply[24..26], [0x12, 0x34]);
		assert_eq!(checksum::of(&reply[20..]), 0);

		let client = Ipv6Addr::new(0xfd53, 0x616b, 0x6900, 0, 0, 0, 0, 2);
		let request = vec![128, 0, 0, 0, 0x12, 0x34, 0, 7, 9];
		let packet = ipv6_packet(client, GATEWAY_V6, 58, &request);
		let echo = EchoRequest::parse(&packet).unwrap();
		let reply = echo.reply(&echo.message).unwrap();
		assert_eq!(reply[40], 129);
		assert_eq!(checksum::icmpv6(GATEWAY_V6, client, &reply[40..]), 0);
	}

	#[test]
	fn other_packets_are_not_mistaken_for_pings() {
		let reply = ipv4_packet(Ipv4Addr::new(1, 1, 1, 1), GATEWAY_V4, 1, &[0, 0, 0, 0, 0, 0, 0, 0]);
		assert!(EchoRequest::parse(&reply).is_none());
		assert!(EchoRequest::parse(&[]).is_none());
		assert!(EchoRequest::parse(&[0x45, 0, 0]).is_none());
		assert!(EchoRequest::parse(&[0x60; 10]).is_none());
		let udp = crate::dns::build_udp(GATEWAY_V4, 1, GATEWAY_V4, 2, b"x");
		assert!(EchoRequest::parse(&udp).is_none());
	}

	#[test]
	fn syns_are_recognized_and_reset_with_valid_checksums() {
		let client: [u8; 4] = [10, 233, 77, 2];
		let server: [u8; 4] = [93, 184, 216, 34];
		let mut segment = [0; 20];
		segment[0..2].copy_from_slice(&40000u16.to_be_bytes());
		segment[2..4].copy_from_slice(&443u16.to_be_bytes());
		segment[4..8].copy_from_slice(&1000u32.to_be_bytes());
		segment[12] = 5 << 4;
		segment[13] = 0x02;
		let packet = ipv4_packet(client.into(), server.into(), 6, &segment);

		let syn = TcpSyn::parse(&packet).unwrap();
		assert_eq!(syn.src, "10.233.77.2:40000".parse().unwrap());
		assert_eq!(syn.dst, "93.184.216.34:443".parse().unwrap());

		let reset = syn.reset();
		assert_eq!(checksum::of(&reset[..20]), 0);
		assert_eq!(checksum::tcp_v4(server, client, &reset[20..]), 0);
		assert_eq!(reset[20 + 13], 0x14);
		assert_eq!(reset[28..32], 1001u32.to_be_bytes());

		// A SYN-ACK and a plain ACK aren't new connections.
		segment[13] = 0x12;
		assert!(TcpSyn::parse(&ipv4_packet(client.into(), server.into(), 6, &segment)).is_none());
		segment[13] = 0x10;
		assert!(TcpSyn::parse(&ipv4_packet(client.into(), server.into(), 6, &segment)).is_none());

		let client = Ipv6Addr::new(0xfd53, 0x616b, 0x6900, 0, 0, 0, 0, 2);
		let server = Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111);
		segment[13] = 0x02;
		let syn = TcpSyn::parse(&ipv6_packet(client, server, 6, &segment)).unwrap();
		let reset = syn.reset();
		assert_eq!(checksum::tcp_v6(server, client, &reset[40..]), 0);
	}
}
