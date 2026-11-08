//! Bluetooth, behind the same small API on each platform.
//!
//! A server advertises a service, publishes a BLE L2CAP channel, and offers one GATT characteristic
//! holding the channel's number. Reading it needs an encrypted link, so that read is also what
//! makes the two machines pair. A client scans for the service, reads the characteristic, opens
//! the channel, and from then on it's just a byte stream.

use anyhow::{Result, bail};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, sleep_until};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Scanner, Service};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Scanner, Service, take_responsibility};

/// A byte stream to a peer.
pub trait LinkStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> LinkStream for T {}
pub type BoxLink = Box<dyn LinkStream>;

/// How long to wait for a first server, and then for any others, when choosing automatically.
const FIND_TIMEOUT: Duration = Duration::from_secs(20);
const SETTLE_TIME: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default)]
pub struct Options {
	/// Linux only: macOS has one adapter, and asks about pairing itself.
	#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
	pub adapter: Option<String>,
	#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
	pub accept_pairing: bool,
	pub name: Option<String>,
}

/// A client arriving at a server.
pub struct Incoming {
	pub stream: BoxLink,
	pub peer: String,
}

/// A server found by scanning.
#[derive(Clone, Debug)]
pub struct Peer {
	pub id: String,
	pub name: Option<String>,
	pub rssi: Option<i16>,
}

impl Peer {
	pub fn label(&self) -> String {
		self.name.clone().unwrap_or_else(|| self.id.clone())
	}

	pub fn describe(&self) -> String {
		let name = self.name.as_deref().unwrap_or("(no name)");
		match self.rssi {
			Some(rssi) => format!("{name}  {}  {rssi} dBm", self.id),
			None => format!("{name}  {}", self.id),
		}
	}

	fn matches(&self, selector: &str) -> bool {
		let selector = selector.to_lowercase();
		self.id.to_lowercase() == selector
			|| self.name.as_ref().is_some_and(|name| name.to_lowercase().contains(&selector))
	}
}

/// An open link to a server.
pub struct Link {
	pub stream: BoxLink,
	/// Whatever has to stay alive to keep the connection up.
	pub keep_alive: Box<dyn std::any::Any + Send>,
}

/// Finds the server to use: the one asked for, or the only one around. Having to guess between
/// several would risk sending someone's traffic through the wrong machine, so that's an error.
pub async fn pick_peer(scanner: &mut Scanner, selector: Option<&str>) -> Result<Peer> {
	let give_up = Instant::now() + FIND_TIMEOUT;
	let mut settled = give_up;
	let mut found: Vec<Peer> = Vec::new();
	loop {
		tokio::select! {
			peer = scanner.next() => {
				let peer = peer?;
				if selector.is_some_and(|selector| peer.matches(selector)) {
					return Ok(peer);
				}
				if found.is_empty() {
					settled = Instant::now() + SETTLE_TIME;
				}
				// The same server again means more is known about it, like its name.
				match found.iter_mut().find(|other| other.id == peer.id) {
					Some(other) => *other = peer,
					None => found.push(peer),
				}
			}
			() = sleep_until(settled) => break,
		}
	}

	match (selector, found.len()) {
		(Some(selector), 0) => bail!("couldn't find a Saki server called {selector}"),
		(Some(selector), _) => bail!("couldn't find a Saki server called {selector}. Found:\n{}", list(&found)),
		(None, 0) => bail!("couldn't find a Saki server. Is `saki server` running, and is it close enough?"),
		(None, 1) => Ok(found.remove(0)),
		(None, _) => bail!("found more than one Saki server, so pick one by name or ID:\n{}", list(&found)),
	}
}

fn list(peers: &[Peer]) -> String {
	peers.iter().map(|peer| format!("  {}", peer.describe())).collect::<Vec<_>>().join("\n")
}
