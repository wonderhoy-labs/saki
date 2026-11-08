//! Runs the two ends of a link once a transport has produced a byte stream.

use crate::dns::{self, DnsResolver};
use crate::nat::{LINK_QUEUE_PACKETS, Nat, NatConfig};
use crate::proto::{self, ByeReason, Frame, RESOLVER_V4, VERSION, Welcome};
use bytes::{Bytes, BytesMut};
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracing::{debug, trace};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const PING_INTERVAL: Duration = Duration::from_secs(10);
/// A link with nothing arriving for this long is dead. Each side pings, so an idle link still has
/// traffic. Bluetooth can stall for several seconds without actually having failed.
const LINK_TIMEOUT: Duration = Duration::from_secs(35);
/// Frames are gathered up to this size before writing, since each write can cost a radio event.
const WRITE_BATCH_BYTES: usize = 8 * 1024;

/// Counters for one link, safe to read from anywhere.
#[derive(Debug, Default)]
pub struct Stats {
	pub bytes_sent: AtomicU64,
	pub bytes_received: AtomicU64,
	/// The latest ping time in milliseconds, or zero before the first reply.
	pub rtt_ms: AtomicU32,
}

/// Runs the server's half of the handshake.
pub async fn accept_handshake<S>(stream: &mut S, welcome: &Welcome) -> io::Result<()>
where
	S: AsyncRead + AsyncWrite + Unpin,
{
	let hello = timeout(HANDSHAKE_TIMEOUT, proto::read_frame(stream))
		.await
		.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the client didn't say hello"))??;
	let Frame::Hello { version } = hello else {
		return Err(io::Error::new(io::ErrorKind::InvalidData, "expected a hello"));
	};
	if version != VERSION {
		let _ = proto::write_frame(stream, &Frame::Bye(ByeReason::VersionMismatch)).await;
		return Err(io::Error::new(
			io::ErrorKind::InvalidData,
			format!("the client speaks protocol {version}, and this is {VERSION}. Update the older one"),
		));
	}
	proto::write_frame(stream, &Frame::Welcome(welcome.clone())).await
}

/// Runs the client's half of the handshake.
pub async fn client_handshake<S>(stream: &mut S) -> io::Result<Welcome>
where
	S: AsyncRead + AsyncWrite + Unpin,
{
	proto::write_frame(stream, &Frame::Hello { version: VERSION }).await?;
	let reply = timeout(HANDSHAKE_TIMEOUT, proto::read_frame(stream))
		.await
		.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the server didn't answer"))??;
	match reply {
		Frame::Welcome(welcome) => Ok(welcome),
		Frame::Bye(ByeReason::VersionMismatch) => Err(io::Error::new(
			io::ErrorKind::InvalidData,
			"the server speaks a different protocol version. Update the older one",
		)),
		other => Err(io::Error::new(io::ErrorKind::InvalidData, format!("expected a welcome, got {other:?}"))),
	}
}

/// Serves one client until the link ends or `stop` resolves.
pub async fn serve_client<S>(
	mut stream: S,
	config: Arc<NatConfig>,
	stats: Arc<Stats>,
	stop: impl Future<Output = ()>,
) -> io::Result<()>
where
	S: AsyncRead + AsyncWrite + Unpin,
{
	accept_handshake(&mut stream, &Welcome::new(config.upstream_dns)).await?;
	let mut nat = Nat::start(config);
	let input = nat.input();
	let mut output = nat.take_output().expect("a new Nat still has its output");
	pump(stream, &mut output, move |packet| input.send(packet), stats, stop).await
}

/// A client's TUN device, kept open across links so that apps' routes survive a reconnection.
pub struct ClientDevice {
	resolver: Arc<Mutex<DnsResolver>>,
	from_device: mpsc::Receiver<Bytes>,
	to_device: mpsc::Sender<Bytes>,
	tasks: [JoinHandle<io::Result<()>>; 2],
}

impl ClientDevice {
	/// Starts moving packets to and from a device, which reads and writes one IP packet at a time.
	pub fn new<T>(device: T, welcome: &Welcome) -> Self
	where
		T: AsyncRead + AsyncWrite + Send + 'static,
	{
		let resolver = Arc::new(Mutex::new(DnsResolver::new(welcome.gateway_v4, RESOLVER_V4, welcome.upstream_dns)));
		let (to_link, from_device) = mpsc::channel::<Bytes>(LINK_QUEUE_PACKETS);
		let (to_device, mut device_queue) = mpsc::channel::<Bytes>(LINK_QUEUE_PACKETS);
		let (mut device_reader, mut device_writer) = tokio::io::split(device);

		let reader_resolver = resolver.clone();
		let reader_to_device = to_device.clone();
		let mtu = usize::from(welcome.mtu);
		let read_task = tokio::spawn(async move {
			let mut buffer = vec![0; mtu + 64];
			loop {
				let len = device_reader.read(&mut buffer).await?;
				if len == 0 {
					return Ok(());
				}
				let packet = &buffer[..len];
				let outcome = reader_resolver.lock().unwrap().outgoing(packet, Instant::now());
				// Never wait for the link here. The kernel would queue up seconds of packets
				// behind this one, whereas dropping tells TCP to slow down.
				let sent = match outcome {
					dns::Outgoing::Pass => to_link.try_send(Bytes::copy_from_slice(packet)),
					dns::Outgoing::Query(query) => to_link.try_send(Bytes::from(query)),
					dns::Outgoing::Reply(reply) => {
						let _ = reader_to_device.try_send(Bytes::from(reply));
						Ok(())
					}
					dns::Outgoing::Drop => Ok(()),
				};
				match sent {
					Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
					Err(mpsc::error::TrySendError::Full(_)) => trace!("link queue full, dropping a packet"),
					Ok(()) => {}
				}
			}
		});
		let write_task = tokio::spawn(async move {
			while let Some(packet) = device_queue.recv().await {
				device_writer.write_all(&packet).await?;
			}
			Ok(())
		});

		ClientDevice { resolver, from_device, to_device, tasks: [read_task, write_task] }
	}
}

impl Drop for ClientDevice {
	fn drop(&mut self) {
		for task in &self.tasks {
			task.abort();
		}
	}
}

/// Connects a device to a link until it ends or `stop` resolves. The handshake has already happened.
pub async fn run_client<S>(
	stream: S,
	device: &mut ClientDevice,
	welcome: &Welcome,
	stats: Arc<Stats>,
	stop: impl Future<Output = ()>,
) -> io::Result<()>
where
	S: AsyncRead + AsyncWrite + Unpin,
{
	device.resolver.lock().unwrap().set_upstream(welcome.upstream_dns);
	let resolver = device.resolver.clone();
	let to_device = device.to_device.clone();
	let on_packet = move |packet: Bytes| {
		let outcome = resolver.lock().unwrap().incoming(&packet, Instant::now());
		let packet = match outcome {
			dns::Incoming::Pass => packet,
			dns::Incoming::Reply(reply) => Bytes::from(reply),
			dns::Incoming::Drop => return,
		};
		if to_device.try_send(packet).is_err() {
			trace!("device queue full, dropping a packet");
		}
	};
	// Packets that piled up while there was no link are long out of date.
	while device.from_device.try_recv().is_ok() {}
	pump(stream, &mut device.from_device, on_packet, stats, stop).await
}

/// Moves frames in both directions at once until the link ends.
///
/// `outbound` packets are written to the stream, and arriving packets go to `on_packet`, which
/// must not block. The link ends cleanly when `stop` resolves, when `outbound` closes or when the
/// peer says goodbye. It's an error if the stream fails or goes quiet.
pub async fn pump<S, F>(
	stream: S,
	outbound: &mut mpsc::Receiver<Bytes>,
	mut on_packet: F,
	stats: Arc<Stats>,
	stop: impl Future<Output = ()>,
) -> io::Result<()>
where
	S: AsyncRead + AsyncWrite + Unpin,
	F: FnMut(Bytes),
{
	let (mut reader, mut writer) = tokio::io::split(stream);
	let (control_tx, mut control_rx) = mpsc::channel::<Frame>(8);
	let last_heard = Mutex::new(Instant::now());
	let epoch = Instant::now();

	let read_loop = async {
		loop {
			let frame = proto::read_frame(&mut reader).await?;
			*last_heard.lock().unwrap() = Instant::now();
			match frame {
				Frame::Packet(packet) => {
					stats.bytes_received.fetch_add(packet.len() as u64, Ordering::Relaxed);
					on_packet(packet);
				}
				Frame::Ping(nonce) => {
					let _ = control_tx.try_send(Frame::Pong(nonce));
				}
				Frame::Pong(sent_at_ms) => {
					let now_ms = epoch.elapsed().as_millis() as u64;
					stats.rtt_ms.store(now_ms.saturating_sub(sent_at_ms).max(1) as u32, Ordering::Relaxed);
				}
				Frame::Bye(reason) => {
					debug!(?reason, "the peer said goodbye");
					return io::Result::Ok(());
				}
				Frame::Hello { .. } | Frame::Welcome(_) => debug!("ignored a repeated handshake frame"),
			}
		}
	};

	let write_loop = async {
		let mut ticker = interval(PING_INTERVAL);
		ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
		let mut batch = BytesMut::with_capacity(WRITE_BATCH_BYTES * 2);
		loop {
			tokio::select! {
				biased;
				Some(frame) = control_rx.recv() => frame.encode(&mut batch),
				_ = ticker.tick() => {
					if last_heard.lock().unwrap().elapsed() > LINK_TIMEOUT {
						return Err(io::Error::new(io::ErrorKind::TimedOut, "the link went quiet"));
					}
					Frame::Ping(epoch.elapsed().as_millis() as u64).encode(&mut batch);
				}
				packet = outbound.recv() => match packet {
					Some(packet) => push_packet(&mut batch, packet, &stats),
					None => return Ok(()),
				},
			}
			// Take whatever else is ready, so a burst of packets goes out as one write.
			while batch.len() < WRITE_BATCH_BYTES {
				match outbound.try_recv() {
					Ok(packet) => push_packet(&mut batch, packet, &stats),
					Err(_) => break,
				}
			}
			writer.write_all(&batch).await?;
			writer.flush().await?;
			batch.clear();
		}
	};

	let said_goodbye = tokio::select! {
		result = read_loop => { result?; true }
		result = write_loop => { result?; false }
		() = stop => false,
	};
	if !said_goodbye {
		let _ = timeout(Duration::from_secs(1), proto::write_frame(&mut writer, &Frame::Bye(ByeReason::Normal))).await;
	}
	Ok(())
}

fn push_packet(batch: &mut BytesMut, packet: Bytes, stats: &Stats) {
	stats.bytes_sent.fetch_add(packet.len() as u64, Ordering::Relaxed);
	Frame::Packet(packet).encode(batch);
}
