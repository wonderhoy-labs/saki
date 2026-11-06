//! The parts of Saki that don't care which platform they run on.
//!
//! A Saki link is a reliable, ordered, full-duplex byte stream. Over Bluetooth that's a BLE L2CAP
//! connection-oriented channel. [`proto`] defines what travels on it, [`session`] runs the two
//! ends, [`nat`] turns a client's IP packets into ordinary sockets on the server, and [`dns`] is
//! the small resolver that clients run for themselves.

pub mod checksum;
pub mod dns;
pub mod nat;
pub mod proto;
pub mod session;
