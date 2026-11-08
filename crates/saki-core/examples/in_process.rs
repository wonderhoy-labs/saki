//! A Saki server and client in one process, joined by an in-memory pipe instead of Bluetooth.
//!
//! The client looks up one.one.one.one by sending a DNS query through the link as an IP packet.
//! The server's NAT sends it on from an ordinary UDP socket, and the answer comes back the same
//! way. It needs an internet connection:
//!
//! ```bash
//! cargo run -p saki-core --example in_process
//! ```

use bytes::Bytes;
use saki_core::dns::build_udp;
use saki_core::nat::NatConfig;
use saki_core::proto::DEFAULT_DNS;
use saki_core::session::{Stats, client_handshake, pump, serve_client};
use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout;

const SOURCE_PORT: u16 = 40_000;

#[tokio::main]
async fn main() -> io::Result<()> {
	// Any reliable, ordered byte stream will do. Saki itself uses a Bluetooth L2CAP channel.
	let (server_end, mut client_end) = tokio::io::duplex(64 * 1024);

	// The server: one call per client, which ends when the link does.
	let config = Arc::new(NatConfig::new(DEFAULT_DNS));
	let server = tokio::spawn(serve_client(server_end, config, Arc::new(Stats::default()), std::future::pending()));

	// The client: a handshake, then packets both ways until the outbound queue closes.
	let welcome = client_handshake(&mut client_end).await?;
	println!("Joined as {}, with an MTU of {}", welcome.client_v4, welcome.mtu);
	let (to_server, mut outbound) = mpsc::channel::<Bytes>(16);
	let (deliver, mut from_server) = mpsc::unbounded_channel::<Bytes>();
	let client = tokio::spawn(async move {
		let on_packet = move |packet: Bytes| {
			let _ = deliver.send(packet);
		};
		pump(client_end, &mut outbound, on_packet, Arc::new(Stats::default()), std::future::pending()).await
	});

	let sent = Instant::now();
	let query = dns_query("one.one.one.one");
	let packet = build_udp(welcome.client_v4, SOURCE_PORT, welcome.upstream_dns, 53, &query);
	to_server.send(Bytes::from(packet)).await.map_err(|_| io::Error::other("the link closed"))?;
	let reply = timeout(Duration::from_secs(5), from_server.recv())
		.await
		.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer. Is this computer online?"))?
		.ok_or_else(|| io::Error::other("the link closed"))?;
	match first_a_record(&reply) {
		Some(address) => println!("one.one.one.one is {address}, answered in {:.0?}", sent.elapsed()),
		None => println!("The answer had no address in it"),
	}

	// Closing the outbound queue ends the client's half, which says goodbye, and that ends the server's.
	drop(to_server);
	client.await.map_err(io::Error::other)??;
	server.await.map_err(io::Error::other)??;
	Ok(())
}

/// A DNS question for the IPv4 address of `name`.
fn dns_query(name: &str) -> Vec<u8> {
	let mut query = vec![0x5a, 0x4b, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
	for label in name.split('.') {
		query.push(label.len() as u8);
		query.extend_from_slice(label.as_bytes());
	}
	query.extend_from_slice(&[0, 0, 1, 0, 1]);
	query
}

/// The first IPv4 address in the DNS answer inside an IPv4 UDP packet.
fn first_a_record(packet: &[u8]) -> Option<Ipv4Addr> {
	let header = usize::from(packet.first()? & 0x0f) * 4;
	let message = packet.get(header + 8..)?;
	// Skip the question, which is the name in labels, then its type and class.
	let mut at = 12;
	while *message.get(at)? != 0 {
		at += 1 + usize::from(message[at]);
	}
	at += 5;
	let answers = u16::from_be_bytes([*message.get(6)?, *message.get(7)?]);
	for _ in 0..answers {
		// Each answer's name is a two-byte pointer back to the question's.
		let kind = u16::from_be_bytes([*message.get(at + 2)?, *message.get(at + 3)?]);
		let length = usize::from(u16::from_be_bytes([*message.get(at + 10)?, *message.get(at + 11)?]));
		let data = message.get(at + 12..at + 12 + length)?;
		if kind == 1 && length == 4 {
			return Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
		}
		at += 12 + length;
	}
	None
}
