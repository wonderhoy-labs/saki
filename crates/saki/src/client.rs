//! `saki client`: joins a server's network through a TUN device, and keeps the link up.

use crate::bt;
use crate::tunnel::Tunnel;
use crate::{ClientArgs, shutdown_signal};
use anyhow::{Context, Result};
use saki_core::proto::Welcome;
use saki_core::session::{ClientDevice, Stats, client_handshake, run_client};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{info, warn};

const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(15);

pub async fn run(args: ClientArgs) -> Result<()> {
    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let mut stop = stop_rx.clone();
    let stopped = async move {
        let _ = stop.wait_for(|stopped| *stopped).await;
    };
    tokio::pin!(stopped);

    let mut state: Option<(Tunnel, ClientDevice)> = None;
    let mut retry_delay = FIRST_RETRY_DELAY;
    loop {
        let attempt = async {
            let mut link = connect(&args).await?;
            let welcome = client_handshake(&mut link.stream).await.context("the handshake failed")?;
            anyhow::Ok((link, welcome))
        };
        let (link, welcome) = tokio::select! {
            () = &mut stopped => return Ok(()),
            result = attempt => match result {
                Ok(connected) => connected,
                Err(error) if state.is_none() && args.once => return Err(error),
                Err(error) => {
                    warn!("{error:#}. Trying again in {} seconds", retry_delay.as_secs());
                    tokio::select! {
                        () = &mut stopped => return Ok(()),
                        () = sleep(retry_delay) => {}
                    }
                    retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                    continue;
                }
            },
        };
        retry_delay = FIRST_RETRY_DELAY;

        if state.is_none() {
            state = Some(open_device(&args, &welcome)?);
        }
        let (tunnel, device) = state.as_mut().expect("the device was just opened");
        info!(server = %link.peer, device = tunnel.name(), address = %welcome.client_v4, "connected");

        let stats = Arc::new(Stats::default());
        let mut stop = stop_rx.clone();
        let link_stop = async move {
            let _ = stop.wait_for(|stopped| *stopped).await;
        };
        let result = run_client(link.stream, device, &welcome, stats.clone(), link_stop).await;
        drop(link.keep_alive);

        let sent = stats.bytes_sent.load(Ordering::Relaxed);
        let received = stats.bytes_received.load(Ordering::Relaxed);
        match result {
            Ok(()) => info!(sent, received, "disconnected"),
            Err(error) => warn!(sent, received, %error, "the link failed"),
        }
        if *stop_rx.borrow() || args.once {
            return Ok(());
        }
    }
}

fn open_device(args: &ClientArgs, welcome: &Welcome) -> Result<(Tunnel, ClientDevice)> {
    let (tunnel, device) = Tunnel::open(args.dev.as_deref(), welcome, !args.no_routes, !args.no_dns)?;
    Ok((tunnel, ClientDevice::new(device, welcome)))
}

/// What a client needs in order to reach a server, however it gets there.
pub struct Connected {
    pub stream: bt::BoxLink,
    pub peer: String,
    /// Dropping this ends the connection.
    pub keep_alive: Option<Box<dyn std::any::Any + Send>>,
}

async fn connect(args: &ClientArgs) -> Result<Connected> {
    open_link(args.dev_tcp, args.server.as_deref(), &args.bluetooth.options()).await
}

pub async fn open_link(
    dev_tcp: Option<SocketAddr>,
    selector: Option<&str>,
    options: &bt::Options,
) -> Result<Connected> {
    if let Some(address) = dev_tcp {
        let stream = TcpStream::connect(address).await.with_context(|| format!("couldn't reach {address}"))?;
        stream.set_nodelay(true)?;
        return Ok(Connected { stream: Box::new(stream), peer: address.to_string(), keep_alive: None });
    }
    let mut scanner = bt::Scanner::start(options).await?;
    let peer = bt::pick_peer(&mut scanner, selector).await?;
    info!(server = %peer.describe(), "connecting");
    let link = scanner.connect(&peer).await?;
    Ok(Connected { stream: link.stream, peer: peer.label(), keep_alive: Some(link.keep_alive) })
}
