//! `saki server`: accepts clients and gives each one its own NAT.

use crate::bt;
use crate::{ServerArgs, shutdown_signal};
use anyhow::Result;
use saki_core::nat::NatConfig;
use saki_core::session::{Stats, serve_client};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tracing::{info, warn};

pub async fn run(args: ServerArgs) -> Result<()> {
	let mut config = NatConfig::new(args.dns);
	config.device = args.dev.clone();
	config.block_lan = args.no_lan;
	config.check_device()?;
	let config = Arc::new(config);

	let (incoming_tx, mut incoming) = mpsc::channel::<bt::Incoming>(4);

	// The Bluetooth service has to stay alive for as long as the server does.
	let _service = match args.dev_tcp {
		Some(address) => {
			let listener = TcpListener::bind(address).await?;
			warn!(%address, "listening on TCP with no pairing. Anyone who can connect gets in");
			tokio::spawn(accept_tcp(listener, incoming_tx));
			None
		}
		None => {
			let mut options = args.bluetooth.options();
			options.name = args.name.clone();
			let service = bt::Service::start(&options, incoming_tx).await?;
			info!(name = %service.name(), "advertising over Bluetooth");
			Some(service)
		}
	};
	match &args.dev {
		Some(device) => info!(%device, dns = %args.dns, "clients' traffic goes through this device"),
		None => info!(dns = %args.dns, "clients' traffic follows this machine's routes"),
	}
	info!("waiting for clients. Press Ctrl-C to stop");

	let (stop_tx, stop_rx) = watch::channel(false);
	let mut sessions = JoinSet::new();
	let shutdown = shutdown_signal();
	tokio::pin!(shutdown);
	loop {
		tokio::select! {
			() = &mut shutdown => break,
			Some(client) = incoming.recv() => {
				sessions.spawn(serve_one(client, config.clone(), stop_rx.clone()));
			}
			Some(_) = sessions.join_next() => {}
		}
	}

	info!("stopping");
	let _ = stop_tx.send(true);
	while sessions.join_next().await.is_some() {}
	Ok(())
}

async fn serve_one(client: bt::Incoming, config: Arc<NatConfig>, mut stop: watch::Receiver<bool>) {
	let bt::Incoming { stream, peer } = client;
	info!(%peer, "client connected");
	let stats = Arc::new(Stats::default());
	let stopped = async move {
		let _ = stop.wait_for(|stopped| *stopped).await;
	};
	let result = serve_client(stream, config, stats.clone(), stopped).await;
	let sent = stats.bytes_sent.load(Ordering::Relaxed);
	let received = stats.bytes_received.load(Ordering::Relaxed);
	match result {
		Ok(()) => info!(%peer, sent, received, "client disconnected"),
		Err(error) => warn!(%peer, sent, received, %error, "client's link failed"),
	}
}

async fn accept_tcp(listener: TcpListener, incoming: mpsc::Sender<bt::Incoming>) {
	loop {
		match listener.accept().await {
			Ok((stream, address)) => {
				let _ = stream.set_nodelay(true);
				let client = bt::Incoming { stream: Box::new(stream), peer: address.to_string() };
				if incoming.send(client).await.is_err() {
					return;
				}
			}
			Err(error) => warn!(%error, "couldn't accept a connection"),
		}
	}
}
