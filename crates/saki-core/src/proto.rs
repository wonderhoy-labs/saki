//! The Saki wire protocol. `docs/protocol.md` describes the same thing for people.
//!
//! Everything on a link is a frame: a one-byte type, a two-byte big-endian payload length, then
//! the payload. The Android app has its own copy of this in Kotlin. The byte strings in this
//! file's tests are repeated in its tests, so the two can't drift apart unnoticed.

use bytes::{BufMut, Bytes, BytesMut};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Advertised by every Saki server, and the GATT service that holds [`INFO_UUID`].
pub const SERVICE_UUID: &str = "53414b49-a776-4be4-9cf4-8ac46746a50f";
/// A characteristic that needs an encrypted link to read, which is also what starts pairing.
pub const INFO_UUID: &str = "53414b49-a777-4be4-9cf4-8ac46746a50f";

pub const MAGIC: [u8; 4] = *b"SAKI";
pub const VERSION: u8 = 1;
/// The IP MTU inside the tunnel. 1280 is the smallest IPv6 allows.
pub const MTU: u16 = 1280;

/// The resolver address clients hand to their operating system. It only exists on the client.
pub const GATEWAY_V4: Ipv4Addr = Ipv4Addr::new(10, 233, 77, 1);
pub const CLIENT_V4: Ipv4Addr = Ipv4Addr::new(10, 233, 77, 2);
/// The source address of the client resolver's own queries. Nothing else sends from it, so its
/// replies can be picked out of the tunnel without colliding with an app's ports.
pub const RESOLVER_V4: Ipv4Addr = Ipv4Addr::new(10, 233, 77, 3);
pub const PREFIX_V4: u8 = 24;
pub const GATEWAY_V6: Ipv6Addr = Ipv6Addr::new(0xfd53, 0x616b, 0x6900, 0, 0, 0, 0, 1);
pub const CLIENT_V6: Ipv6Addr = Ipv6Addr::new(0xfd53, 0x616b, 0x6900, 0, 0, 0, 0, 2);
pub const PREFIX_V6: u8 = 64;
pub const DEFAULT_DNS: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

const TYPE_HELLO: u8 = 1;
const TYPE_WELCOME: u8 = 2;
const TYPE_PACKET: u8 = 3;
const TYPE_PING: u8 = 4;
const TYPE_PONG: u8 = 5;
const TYPE_BYE: u8 = 6;

const HEADER_LEN: usize = 3;
const WELCOME_LEN: usize = 2 + 4 + 4 + 1 + 16 + 16 + 1 + 4;

/// What a server tells a client about the network it's joining.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcome {
	pub mtu: u16,
	pub client_v4: Ipv4Addr,
	pub gateway_v4: Ipv4Addr,
	pub prefix_v4: u8,
	pub client_v6: Ipv6Addr,
	pub gateway_v6: Ipv6Addr,
	pub prefix_v6: u8,
	pub upstream_dns: Ipv4Addr,
}

impl Welcome {
	pub fn new(upstream_dns: Ipv4Addr) -> Self {
		Welcome {
			mtu: MTU,
			client_v4: CLIENT_V4,
			gateway_v4: GATEWAY_V4,
			prefix_v4: PREFIX_V4,
			client_v6: CLIENT_V6,
			gateway_v6: GATEWAY_V6,
			prefix_v6: PREFIX_V6,
			upstream_dns,
		}
	}
}

/// Why a link is ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByeReason {
	Normal,
	VersionMismatch,
	Unknown(u8),
}

impl ByeReason {
	fn code(self) -> u8 {
		match self {
			ByeReason::Normal => 0,
			ByeReason::VersionMismatch => 1,
			ByeReason::Unknown(code) => code,
		}
	}

	fn from_code(code: u8) -> Self {
		match code {
			0 => ByeReason::Normal,
			1 => ByeReason::VersionMismatch,
			code => ByeReason::Unknown(code),
		}
	}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
	Hello { version: u8 },
	Welcome(Welcome),
	Packet(Bytes),
	Ping(u64),
	Pong(u64),
	Bye(ByeReason),
}

impl Frame {
	/// Appends this frame to `out`.
	pub fn encode(&self, out: &mut BytesMut) {
		match self {
			Frame::Hello { version } => {
				put_header(out, TYPE_HELLO, MAGIC.len() + 1);
				out.put_slice(&MAGIC);
				out.put_u8(*version);
			}
			Frame::Welcome(w) => {
				put_header(out, TYPE_WELCOME, WELCOME_LEN);
				out.put_u16(w.mtu);
				out.put_slice(&w.client_v4.octets());
				out.put_slice(&w.gateway_v4.octets());
				out.put_u8(w.prefix_v4);
				out.put_slice(&w.client_v6.octets());
				out.put_slice(&w.gateway_v6.octets());
				out.put_u8(w.prefix_v6);
				out.put_slice(&w.upstream_dns.octets());
			}
			Frame::Packet(packet) => {
				put_header(out, TYPE_PACKET, packet.len());
				out.put_slice(packet);
			}
			Frame::Ping(nonce) => {
				put_header(out, TYPE_PING, 8);
				out.put_u64(*nonce);
			}
			Frame::Pong(nonce) => {
				put_header(out, TYPE_PONG, 8);
				out.put_u64(*nonce);
			}
			Frame::Bye(reason) => {
				put_header(out, TYPE_BYE, 1);
				out.put_u8(reason.code());
			}
		}
	}

	/// Decodes a frame's payload. Unknown types come back as `None`, so that a later protocol
	/// version can add frames that older peers just skip.
	pub fn decode(kind: u8, payload: Bytes) -> io::Result<Option<Frame>> {
		let frame = match kind {
			TYPE_HELLO => {
				if payload.len() < MAGIC.len() + 1 || payload[..MAGIC.len()] != MAGIC {
					return Err(invalid("not a Saki hello"));
				}
				Frame::Hello { version: payload[MAGIC.len()] }
			}
			TYPE_WELCOME => {
				// Longer is fine: a later version may append fields.
				if payload.len() < WELCOME_LEN {
					return Err(invalid("welcome frame is too short"));
				}
				Frame::Welcome(Welcome {
					mtu: u16::from_be_bytes([payload[0], payload[1]]),
					client_v4: v4(&payload[2..]),
					gateway_v4: v4(&payload[6..]),
					prefix_v4: payload[10],
					client_v6: v6(&payload[11..]),
					gateway_v6: v6(&payload[27..]),
					prefix_v6: payload[43],
					upstream_dns: v4(&payload[44..]),
				})
			}
			TYPE_PACKET => Frame::Packet(payload),
			TYPE_PING | TYPE_PONG => {
				let nonce = payload
					.get(..8)
					.and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
					.map(u64::from_be_bytes)
					.ok_or_else(|| invalid("ping frame is too short"))?;
				if kind == TYPE_PING { Frame::Ping(nonce) } else { Frame::Pong(nonce) }
			}
			TYPE_BYE => {
				let code = *payload.first().ok_or_else(|| invalid("bye frame is empty"))?;
				Frame::Bye(ByeReason::from_code(code))
			}
			_ => return Ok(None),
		};
		Ok(Some(frame))
	}
}

fn put_header(out: &mut BytesMut, kind: u8, len: usize) {
	debug_assert!(len <= usize::from(u16::MAX));
	out.reserve(HEADER_LEN + len);
	out.put_u8(kind);
	out.put_u16(len as u16);
}

fn v4(bytes: &[u8]) -> Ipv4Addr {
	Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3])
}

fn v6(bytes: &[u8]) -> Ipv6Addr {
	let mut octets = [0; 16];
	octets.copy_from_slice(&bytes[..16]);
	Ipv6Addr::from(octets)
}

fn invalid(message: &'static str) -> io::Error {
	io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Reads the next frame this version understands.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Frame> {
	loop {
		let mut header = [0; HEADER_LEN];
		reader.read_exact(&mut header).await?;
		let len = usize::from(u16::from_be_bytes([header[1], header[2]]));
		let mut payload = BytesMut::zeroed(len);
		reader.read_exact(&mut payload).await?;
		if let Some(frame) = Frame::decode(header[0], payload.freeze())? {
			return Ok(frame);
		}
	}
}

/// Writes one frame and flushes it. The session's packet path batches instead of using this.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
	let mut out = BytesMut::new();
	frame.encode(&mut out);
	writer.write_all(&out).await?;
	writer.flush().await
}

/// The value of the GATT info characteristic: magic, version, then the L2CAP PSM.
pub fn encode_info(psm: u16) -> [u8; 7] {
	let [high, low] = psm.to_be_bytes();
	[MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], VERSION, high, low]
}

/// Reads the info characteristic, returning the peer's protocol version and PSM.
pub fn decode_info(value: &[u8]) -> io::Result<(u8, u16)> {
	if value.len() < 7 || value[..4] != MAGIC {
		return Err(invalid("not a Saki info characteristic"));
	}
	Ok((value[4], u16::from_be_bytes([value[5], value[6]])))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn encoded(frame: &Frame) -> Vec<u8> {
		let mut out = BytesMut::new();
		frame.encode(&mut out);
		out.to_vec()
	}

	// The Android app's ProtoTest checks the same bytes. Change them together.
	const WELCOME_BYTES: &[u8] = &[
		2, 0, 48, // header
		0x05, 0x00, // mtu
		10, 233, 77, 2, // client v4
		10, 233, 77, 1,  // gateway v4
		24, // prefix v4
		0xfd, 0x53, 0x61, 0x6b, 0x69, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, // client v6
		0xfd, 0x53, 0x61, 0x6b, 0x69, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,  // gateway v6
		64, // prefix v6
		1, 1, 1, 1, // upstream dns
	];

	#[test]
	fn frames_match_the_documented_bytes() {
		assert_eq!(encoded(&Frame::Hello { version: 1 }), [1, 0, 5, b'S', b'A', b'K', b'I', 1]);
		assert_eq!(encoded(&Frame::Welcome(Welcome::new(DEFAULT_DNS))), WELCOME_BYTES);
		assert_eq!(encoded(&Frame::Packet(Bytes::from_static(&[0x45, 0x00]))), [3, 0, 2, 0x45, 0x00]);
		assert_eq!(encoded(&Frame::Ping(0x0102030405060708)), [4, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8]);
		assert_eq!(encoded(&Frame::Pong(9)), [5, 0, 8, 0, 0, 0, 0, 0, 0, 0, 9]);
		assert_eq!(encoded(&Frame::Bye(ByeReason::VersionMismatch)), [6, 0, 1, 1]);
		assert_eq!(encode_info(0x0081), [b'S', b'A', b'K', b'I', 1, 0x00, 0x81]);
	}

	#[tokio::test]
	async fn frames_survive_a_round_trip() {
		let frames = [
			Frame::Hello { version: VERSION },
			Frame::Welcome(Welcome::new(Ipv4Addr::new(9, 9, 9, 9))),
			Frame::Packet(Bytes::from(vec![7; 1280])),
			Frame::Ping(u64::MAX),
			Frame::Pong(0),
			Frame::Bye(ByeReason::Normal),
			Frame::Bye(ByeReason::Unknown(200)),
		];
		let mut wire = BytesMut::new();
		for frame in &frames {
			frame.encode(&mut wire);
		}
		let mut reader = &wire[..];
		for frame in &frames {
			assert_eq!(&read_frame(&mut reader).await.unwrap(), frame);
		}
	}

	#[tokio::test]
	async fn unknown_frames_are_skipped() {
		let mut reader: &[u8] = &[99, 0, 2, 1, 2, 4, 0, 8, 0, 0, 0, 0, 0, 0, 0, 5];
		assert_eq!(read_frame(&mut reader).await.unwrap(), Frame::Ping(5));
	}

	#[tokio::test]
	async fn a_bad_hello_is_an_error() {
		let mut reader: &[u8] = &[1, 0, 5, b'N', b'O', b'P', b'E', 1];
		assert!(read_frame(&mut reader).await.is_err());
	}

	#[test]
	fn welcome_accepts_trailing_fields() {
		let mut payload = WELCOME_BYTES[3..].to_vec();
		payload.push(0xff);
		let frame = Frame::decode(TYPE_WELCOME, Bytes::from(payload)).unwrap();
		assert_eq!(frame, Some(Frame::Welcome(Welcome::new(DEFAULT_DNS))));
	}

	#[test]
	fn info_round_trips_and_rejects_garbage() {
		assert_eq!(decode_info(&encode_info(0x1234)).unwrap(), (VERSION, 0x1234));
		assert!(decode_info(b"SAKI").is_err());
		assert!(decode_info(b"NOPE\x01\x00\x81").is_err());
	}
}
