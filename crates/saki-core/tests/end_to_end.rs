//! Runs a real server session over an in-memory link, with a hand-driven client on the other end.
//! The client's packets go through the real NAT to real sockets on the loopback interface.

use bytes::Bytes;
use etherparse::{IpNumber, Ipv4Header, PacketBuilder, SlicedPacket, TcpHeader, TransportSlice};
use saki_core::dns::build_udp;
use saki_core::nat::NatConfig;
use saki_core::proto::{self, CLIENT_V4, DEFAULT_DNS, Frame, GATEWAY_V4, VERSION, Welcome};
use saki_core::session::{self, Stats, client_handshake};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

struct Client {
	to_link: mpsc::Sender<Bytes>,
	from_link: mpsc::UnboundedReceiver<Bytes>,
	welcome: Welcome,
	stop: Option<oneshot::Sender<()>>,
	server: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn connect(config: NatConfig) -> Client {
	let (client_end, server_end) = duplex(64 * 1024);
	let (stop, stopped) = oneshot::channel::<()>();
	let server =
		tokio::spawn(session::serve_client(server_end, Arc::new(config), Arc::new(Stats::default()), async move {
			let _ = stopped.await;
		}));
	let (welcome, to_link, from_link) = start_client(client_end).await;
	Client { to_link, from_link, welcome, stop: Some(stop), server }
}

async fn start_client(mut stream: DuplexStream) -> (Welcome, mpsc::Sender<Bytes>, mpsc::UnboundedReceiver<Bytes>) {
	let welcome = client_handshake(&mut stream).await.unwrap();
	let (to_link, mut outbound) = mpsc::channel(64);
	let (delivered, from_link) = mpsc::unbounded_channel();
	tokio::spawn(async move {
		session::pump(
			stream,
			&mut outbound,
			move |packet| {
				let _ = delivered.send(packet);
			},
			Arc::new(Stats::default()),
			std::future::pending::<()>(),
		)
		.await
	});
	(welcome, to_link, from_link)
}

fn nat_for_tests() -> NatConfig {
	NatConfig { allow_loopback: true, ..NatConfig::new(DEFAULT_DNS) }
}

/// A very small TCP client: enough to open a connection, move data and close.
struct TcpProbe {
	src_port: u16,
	dst: SocketAddr,
	seq: u32,
	ack: u32,
}

impl TcpProbe {
	fn segment(&self, build: impl FnOnce(TcpHeader) -> TcpHeader, payload: &[u8]) -> Bytes {
		let SocketAddr::V4(dst) = self.dst else { panic!("IPv4 only") };
		let mut header = TcpHeader::new(self.src_port, dst.port(), self.seq, 64240);
		header.acknowledgment_number = self.ack;
		header = build(header);
		let mut ip = Ipv4Header::new(0, 64, IpNumber::TCP, CLIENT_V4.octets(), dst.ip().octets()).unwrap();
		ip.set_payload_len(header.header_len() + payload.len()).unwrap();
		header.checksum = header.calc_checksum_ipv4(&ip, payload).unwrap();
		let mut packet = Vec::new();
		ip.write(&mut packet).unwrap();
		header.write(&mut packet).unwrap();
		packet.extend_from_slice(payload);
		Bytes::from(packet)
	}
}

/// The next TCP segment from the link: its flags, sequence, acknowledgment and payload.
async fn next_tcp(client: &mut Client) -> (bool, bool, bool, u32, Vec<u8>) {
	loop {
		let packet = timeout(WAIT, client.from_link.recv()).await.expect("timed out waiting for TCP").unwrap();
		let sliced = SlicedPacket::from_ip(&packet).unwrap();
		if let Some(TransportSlice::Tcp(tcp)) = sliced.transport {
			return (tcp.syn(), tcp.fin(), tcp.rst(), tcp.sequence_number(), tcp.payload().to_vec());
		}
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_works_in_both_directions_at_once() {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let target = listener.local_addr().unwrap();
	let echo = tokio::spawn(async move {
		let (mut socket, _) = listener.accept().await.unwrap();
		let mut buffer = [0; 1024];
		let len = socket.read(&mut buffer).await.unwrap();
		socket.write_all(b"pong:").await.unwrap();
		socket.write_all(&buffer[..len]).await.unwrap();
		socket.shutdown().await.unwrap();
	});

	let mut client = connect(nat_for_tests()).await;
	let mut probe = TcpProbe { src_port: 40000, dst: target, seq: 1000, ack: 0 };

	let syn = probe.segment(
		|mut h| {
			h.syn = true;
			h
		},
		&[],
	);
	client.to_link.send(syn).await.unwrap();
	let (is_syn, _, _, server_seq, _) = next_tcp(&mut client).await;
	assert!(is_syn, "expected a SYN-ACK");
	probe.seq += 1;
	probe.ack = server_seq.wrapping_add(1);
	let ack = probe.segment(
		|mut h| {
			h.ack = true;
			h
		},
		&[],
	);
	client.to_link.send(ack).await.unwrap();

	let request = probe.segment(
		|mut h| {
			h.ack = true;
			h.psh = true;
			h
		},
		b"ping",
	);
	client.to_link.send(request).await.unwrap();
	probe.seq += 4;

	let mut received = Vec::new();
	loop {
		let (_, fin, rst, seq, payload) = next_tcp(&mut client).await;
		assert!(!rst, "the connection was reset");
		if !payload.is_empty() && seq == probe.ack {
			probe.ack = probe.ack.wrapping_add(payload.len() as u32);
			received.extend_from_slice(&payload);
			let ack = probe.segment(
				|mut h| {
					h.ack = true;
					h
				},
				&[],
			);
			client.to_link.send(ack).await.unwrap();
		}
		if fin {
			break;
		}
	}
	assert_eq!(received, b"pong:ping");
	echo.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn off_limits_destinations_get_a_reset() {
	// Loopback is off limits with the default settings.
	let mut client = connect(NatConfig::new(DEFAULT_DNS)).await;
	let probe = TcpProbe { src_port: 40001, dst: "127.0.0.1:9".parse().unwrap(), seq: 1, ack: 0 };
	let syn = probe.segment(
		|mut h| {
			h.syn = true;
			h
		},
		&[],
	);
	client.to_link.send(syn).await.unwrap();

	let (is_syn, _, rst, ..) = next_tcp(&mut client).await;
	assert!(!is_syn, "the stack answered a connection it should have refused");
	assert!(rst, "expected a reset");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_learns_when_a_destination_refuses_the_connection() {
	// Find a port with nothing listening on it.
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let target = listener.local_addr().unwrap();
	drop(listener);

	let mut client = connect(nat_for_tests()).await;
	let probe = TcpProbe { src_port: 40002, dst: target, seq: 1, ack: 0 };
	let syn = probe.segment(
		|mut h| {
			h.syn = true;
			h
		},
		&[],
	);
	client.to_link.send(syn).await.unwrap();

	let (is_syn, _, rst, ..) = next_tcp(&mut client).await;
	assert!(!is_syn, "the client was told it connected");
	assert!(rst, "expected a reset");
}

/// A client that closes its sending side still has to get the answer, as with `nc` or an old
/// HTTP client.
#[tokio::test(flavor = "multi_thread")]
async fn a_half_closed_connection_still_gets_its_answer() {
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let target = listener.local_addr().unwrap();
	let server = tokio::spawn(async move {
		let (mut socket, _) = listener.accept().await.unwrap();
		let mut request = Vec::new();
		socket.read_to_end(&mut request).await.unwrap();
		assert_eq!(request, b"question");
		socket.write_all(b"answer").await.unwrap();
		socket.shutdown().await.unwrap();
	});

	let mut client = connect(nat_for_tests()).await;
	let mut probe = TcpProbe { src_port: 40004, dst: target, seq: 500, ack: 0 };
	let syn = probe.segment(
		|mut h| {
			h.syn = true;
			h
		},
		&[],
	);
	client.to_link.send(syn).await.unwrap();
	let (is_syn, _, _, server_seq, _) = next_tcp(&mut client).await;
	assert!(is_syn);
	probe.seq += 1;
	probe.ack = server_seq.wrapping_add(1);

	let request = probe.segment(
		|mut h| {
			h.ack = true;
			h.psh = true;
			h
		},
		b"question",
	);
	client.to_link.send(request).await.unwrap();
	probe.seq += 8;
	let fin = probe.segment(
		|mut h| {
			h.ack = true;
			h.fin = true;
			h
		},
		&[],
	);
	client.to_link.send(fin).await.unwrap();
	probe.seq += 1;

	let mut received = Vec::new();
	loop {
		let (_, fin, rst, seq, payload) = next_tcp(&mut client).await;
		assert!(!rst, "the connection was reset");
		if !payload.is_empty() && seq == probe.ack {
			probe.ack = probe.ack.wrapping_add(payload.len() as u32);
			received.extend_from_slice(&payload);
		}
		if fin {
			break;
		}
	}
	assert_eq!(received, b"answer");
	timeout(WAIT, server).await.unwrap().unwrap();
}

/// Moves a megabyte down to the client while it sends a steady stream up, so that the two
/// directions are busy together on one link.
#[tokio::test(flavor = "multi_thread")]
async fn a_big_download_finishes_while_uploading() {
	const DOWNLOAD: usize = 1024 * 1024;
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let target = listener.local_addr().unwrap();
	let server = tokio::spawn(async move {
		let (socket, _) = listener.accept().await.unwrap();
		let (mut reader, mut writer) = socket.into_split();
		let upload = tokio::spawn(async move {
			let mut total = 0;
			let mut buffer = [0; 4096];
			while let Ok(len) = reader.read(&mut buffer).await {
				if len == 0 {
					break;
				}
				total += len;
			}
			total
		});
		let chunk = vec![0x5a; 8192];
		for _ in 0..DOWNLOAD / chunk.len() {
			writer.write_all(&chunk).await.unwrap();
		}
		writer.shutdown().await.unwrap();
		upload.await.unwrap()
	});

	let mut client = connect(nat_for_tests()).await;
	let mut probe = TcpProbe { src_port: 40003, dst: target, seq: 1000, ack: 0 };
	let syn = probe.segment(
		|mut h| {
			h.syn = true;
			h
		},
		&[],
	);
	client.to_link.send(syn).await.unwrap();
	let (is_syn, _, _, server_seq, _) = next_tcp(&mut client).await;
	assert!(is_syn);
	probe.seq += 1;
	probe.ack = server_seq.wrapping_add(1);

	let mut downloaded = 0;
	let mut uploaded = 0;
	loop {
		// Acknowledge what's arrived so far, and send a little with it.
		let upload = [0x33; 200];
		let segment = probe.segment(
			|mut h| {
				h.ack = true;
				h.psh = true;
				h
			},
			&upload,
		);
		client.to_link.send(segment).await.unwrap();
		probe.seq += upload.len() as u32;
		uploaded += upload.len();

		let (_, fin, rst, seq, payload) = next_tcp(&mut client).await;
		assert!(!rst, "the connection was reset");
		if seq == probe.ack {
			probe.ack = probe.ack.wrapping_add(payload.len() as u32);
			downloaded += payload.len();
		}
		if fin && downloaded == DOWNLOAD {
			break;
		}
	}

	probe.ack = probe.ack.wrapping_add(1);
	let fin = probe.segment(
		|mut h| {
			h.ack = true;
			h.fin = true;
			h
		},
		&[],
	);
	client.to_link.send(fin).await.unwrap();

	assert_eq!(timeout(WAIT, server).await.unwrap().unwrap(), uploaded);
}

#[tokio::test(flavor = "multi_thread")]
async fn udp_is_relayed_and_answers_come_back() {
	let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
	let port = socket.local_addr().unwrap().port();
	tokio::spawn(async move {
		let mut buffer = [0; 1500];
		let (len, from) = socket.recv_from(&mut buffer).await.unwrap();
		buffer[..len].reverse();
		socket.send_to(&buffer[..len], from).await.unwrap();
	});

	let mut client = connect(nat_for_tests()).await;
	let packet = build_udp(CLIENT_V4, 5353, Ipv4Addr::LOCALHOST, port, b"abc");
	client.to_link.send(Bytes::from(packet)).await.unwrap();

	let reply = timeout(WAIT, client.from_link.recv()).await.unwrap().unwrap();
	let sliced = SlicedPacket::from_ip(&reply).unwrap();
	let Some(TransportSlice::Udp(udp)) = sliced.transport else { panic!("expected UDP") };
	assert_eq!(udp.source_port(), port);
	assert_eq!(udp.destination_port(), 5353);
	assert_eq!(udp.payload(), b"cba");
}

#[tokio::test(flavor = "multi_thread")]
async fn pinging_the_gateway_is_answered_by_the_server() {
	let mut client = connect(NatConfig::new(DEFAULT_DNS)).await;
	let mut packet = Vec::new();
	PacketBuilder::ipv4(CLIENT_V4.octets(), GATEWAY_V4.octets(), 64)
		.icmpv4_echo_request(0x4242, 7)
		.write(&mut packet, b"hello")
		.unwrap();
	client.to_link.send(Bytes::from(packet)).await.unwrap();

	let reply = timeout(WAIT, client.from_link.recv()).await.unwrap().unwrap();
	let sliced = SlicedPacket::from_ip(&reply).unwrap();
	let Some(TransportSlice::Icmpv4(icmp)) = sliced.transport else { panic!("expected ICMP") };
	assert_eq!(icmp.type_u8(), 0);
	assert_eq!(saki_core::checksum::of(icmp.slice()), 0);
	assert_eq!(&icmp.slice()[4..6], &[0x42, 0x42]);
	assert_eq!(&icmp.slice()[8..], b"hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handshake_hands_over_the_network_settings() {
	let client = connect(NatConfig::new(Ipv4Addr::new(9, 9, 9, 9))).await;
	assert_eq!(client.welcome, Welcome::new(Ipv4Addr::new(9, 9, 9, 9)));
	assert_eq!(client.welcome.client_v4, CLIENT_V4);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_server_says_goodbye() {
	let mut client = connect(NatConfig::new(DEFAULT_DNS)).await;
	client.stop.take().unwrap().send(()).unwrap();
	timeout(WAIT, &mut client.server).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_with_the_wrong_version_is_turned_away() {
	let (mut client_end, server_end) = duplex(1024);
	let server = tokio::spawn(session::serve_client(
		server_end,
		Arc::new(NatConfig::new(DEFAULT_DNS)),
		Arc::new(Stats::default()),
		std::future::pending(),
	));
	proto::write_frame(&mut client_end, &Frame::Hello { version: VERSION + 1 }).await.unwrap();
	let reply = timeout(WAIT, proto::read_frame(&mut client_end)).await.unwrap().unwrap();
	assert!(matches!(reply, Frame::Bye(proto::ByeReason::VersionMismatch)));
	assert!(server.await.unwrap().is_err());
}
