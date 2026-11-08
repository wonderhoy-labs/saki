//! The Saki command-line tool for macOS and Linux.

mod bt;
mod client;
mod probe;
#[cfg(target_os = "linux")]
mod resolv_conf;
mod server;
mod tunnel;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use saki_core::proto::DEFAULT_DNS;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "saki", version, about = "Full-duplex networking over Bluetooth")]
struct Cli {
	/// Log more. Repeat for even more.
	#[arg(short, long, action = clap::ArgAction::Count, global = true)]
	verbose: u8,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Share this machine's network with clients over Bluetooth
	Server(ServerArgs),
	/// Get networking from a Saki server over Bluetooth. Needs root
	Client(ClientArgs),
	/// List the Saki servers in range
	Scan(ScanArgs),
	/// Connect to a server and check the link, without touching the network setup
	Probe(ProbeArgs),
}

#[derive(Args)]
struct BluetoothArgs {
	/// Bluetooth adapter to use (Linux only), like hci1
	#[arg(long, value_name = "NAME")]
	adapter: Option<String>,

	/// Accept pairing requests without asking (Linux only). Anyone in range could then pair
	#[arg(long)]
	accept_pairing: bool,
}

#[derive(Args)]
pub struct ServerArgs {
	/// Send clients' traffic through this network device or TUN, like xray0
	#[arg(long, value_name = "DEVICE")]
	dev: Option<String>,

	/// The name clients see
	#[arg(long)]
	name: Option<String>,

	/// The resolver clients use for DNS
	#[arg(long, value_name = "IPV4", default_value_t = DEFAULT_DNS)]
	dns: Ipv4Addr,

	/// Keep clients off private addresses, like this machine's local network
	#[arg(long)]
	no_lan: bool,

	#[command(flatten)]
	bluetooth: BluetoothArgs,

	/// Listen on TCP instead of Bluetooth. For development: anyone who can connect gets in
	#[arg(long, value_name = "ADDRESS", hide = true)]
	dev_tcp: Option<SocketAddr>,
}

#[derive(Args)]
pub struct ClientArgs {
	/// The server's name or ID, from `saki scan`. Optional when there's only one in range
	server: Option<String>,

	/// Use this TUN device for the Bluetooth network instead of creating one
	#[arg(long, value_name = "DEVICE")]
	dev: Option<String>,

	/// Don't send this machine's traffic into the tunnel. Add your own routes to use it
	#[arg(long)]
	no_routes: bool,

	/// Don't point this machine's DNS at the tunnel
	#[arg(long)]
	no_dns: bool,

	/// Exit when the link drops instead of reconnecting
	#[arg(long)]
	once: bool,

	#[command(flatten)]
	bluetooth: BluetoothArgs,

	/// Connect over TCP instead of Bluetooth. For development
	#[arg(long, value_name = "ADDRESS", hide = true)]
	dev_tcp: Option<SocketAddr>,
}

#[derive(Args)]
pub struct ScanArgs {
	/// How long to look, in seconds
	#[arg(long, default_value_t = 10)]
	seconds: u64,

	#[command(flatten)]
	bluetooth: BluetoothArgs,
}

#[derive(Args)]
pub struct ProbeArgs {
	/// The server's name or ID, from `saki scan`. Optional when there's only one in range
	server: Option<String>,

	/// How many pings to send
	#[arg(long, default_value_t = 5)]
	count: u32,

	#[command(flatten)]
	bluetooth: BluetoothArgs,

	/// Connect over TCP instead of Bluetooth. For development
	#[arg(long, value_name = "ADDRESS", hide = true)]
	dev_tcp: Option<SocketAddr>,
}

impl BluetoothArgs {
	fn options(&self) -> bt::Options {
		bt::Options { adapter: self.adapter.clone(), accept_pairing: self.accept_pairing, name: None }
	}
}

fn main() -> Result<()> {
	// This restarts the process, so it has to happen before anything else does.
	#[cfg(target_os = "macos")]
	bt::take_responsibility();
	run()
}

#[tokio::main]
async fn run() -> Result<()> {
	let cli = Cli::parse();

	let default_level = match cli.verbose {
		0 => "info",
		1 => "saki=debug,saki_core=debug,info",
		_ => "saki=trace,saki_core=trace,info",
	};
	tracing_subscriber::fmt()
		.with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)))
		.with_target(false)
		.init();

	match cli.command {
		Command::Server(args) => server::run(args).await,
		Command::Client(args) => client::run(args).await,
		Command::Probe(args) => probe::run(args).await,
		Command::Scan(args) => scan(args).await,
	}
}

async fn scan(args: ScanArgs) -> Result<()> {
	let mut scanner = bt::Scanner::start(&args.bluetooth.options()).await?;
	println!("Looking for Saki servers for {} seconds...", args.seconds);
	let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
	// A server can show up before its name does, so hold off on printing it until the name arrives.
	let mut nameless: Vec<bt::Peer> = Vec::new();
	let mut found = 0;
	loop {
		tokio::select! {
			peer = scanner.next() => {
				let peer = peer?;
				nameless.retain(|other| other.id != peer.id);
				if peer.name.is_some() {
					println!("{}", peer.describe());
					found += 1;
				} else {
					nameless.push(peer);
				}
			}
			() = tokio::time::sleep_until(deadline) => break,
		}
	}
	for peer in &nameless {
		println!("{}", peer.describe());
	}
	if found + nameless.len() == 0 {
		println!("None found. Is `saki server` running, and is it close enough?");
	}
	Ok(())
}

/// Resolves when the user asks the program to stop.
pub async fn shutdown_signal() {
	use tokio::signal::unix::{SignalKind, signal};
	let mut terminate = signal(SignalKind::terminate()).expect("couldn't listen for SIGTERM");
	tokio::select! {
		_ = tokio::signal::ctrl_c() => {}
		_ = terminate.recv() => {}
	}
}
