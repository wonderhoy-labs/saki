//! Bluetooth on Linux, through BlueZ.

use super::{Incoming, Link, Options, Peer};
use anyhow::{Context, Result, anyhow, bail};
use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::agent::{Agent, AgentHandle, ReqError, ReqResult};
use bluer::gatt::local::{Application, ApplicationHandle, Characteristic, CharacteristicRead, Service as GattService};
use bluer::l2cap::{self, Security, SecurityLevel, Socket, SocketAddr};
use bluer::{Adapter, AdapterEvent, Address, AddressType, Device, DiscoveryFilter, DiscoveryTransport, Session};
use futures::{FutureExt, StreamExt};
use saki_core::proto::{INFO_UUID, SERVICE_UUID, VERSION, decode_info, encode_info};
use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};
use uuid::Uuid;

const SCAN_START_TRIES: u32 = 10;
const SCAN_START_RETRY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(60);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest SDU to accept. A frame has to fit in this, and 2 KiB leaves room above the MTU.
const RECV_MTU: u16 = 2048;

fn service_uuid() -> Uuid {
    SERVICE_UUID.parse().expect("the service UUID is valid")
}

fn info_uuid() -> Uuid {
    INFO_UUID.parse().expect("the info UUID is valid")
}

async fn open_adapter(session: &Session, options: &Options) -> Result<Adapter> {
    let adapter = match &options.adapter {
        Some(name) => session.adapter(name).with_context(|| format!("no Bluetooth adapter called {name}"))?,
        None => session.default_adapter().await.context("couldn't find a Bluetooth adapter. Is bluetoothd running?")?,
    };
    adapter.set_powered(true).await.context("couldn't turn on Bluetooth. Is it blocked by rfkill?")?;
    Ok(adapter)
}

/// Answers BlueZ's pairing questions on this terminal.
async fn register_agent(session: &Session, accept_all: bool) -> Result<AgentHandle> {
    let agent = Agent {
        request_default: true,
        request_confirmation: Some(Box::new(move |request| {
            async move {
                let question =
                    format!("Pair with {}? Check that both screens show {:06}", request.device, request.passkey);
                approve(question, accept_all).await
            }
            .boxed()
        })),
        request_authorization: Some(Box::new(move |request| {
            async move { approve(format!("Pair with {}?", request.device), accept_all).await }.boxed()
        })),
        authorize_service: Some(Box::new(move |request| {
            async move { approve(format!("Let {} use {}?", request.device, request.service), accept_all).await }.boxed()
        })),
        display_passkey: Some(Box::new(|request| {
            async move {
                info!(device = %request.device, "type {:06} on the other device", request.passkey);
                Ok(())
            }
            .boxed()
        })),
        ..Default::default()
    };
    session.register_agent(agent).await.context("couldn't register a pairing agent")
}

async fn approve(question: String, accept_all: bool) -> ReqResult<()> {
    if accept_all {
        info!("{question} Accepting");
        return Ok(());
    }
    let answer = tokio::task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            warn!("{question} Refusing, since nobody's here to answer. Use --accept-pairing to allow it");
            return false;
        }
        print!("{question} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
    });
    match answer.await {
        Ok(true) => Ok(()),
        _ => Err(ReqError::Rejected),
    }
}

/// A running server. Dropping it stops advertising and closes the channel.
pub struct Service {
    name: String,
    _session: Session,
    _agent: AgentHandle,
    _advertisement: AdvertisementHandle,
    _application: ApplicationHandle,
    accept_task: JoinHandle<()>,
}

impl Service {
    pub async fn start(options: &Options, incoming: mpsc::Sender<Incoming>) -> Result<Self> {
        let session = Session::new().await.context("couldn't reach BlueZ. Is bluetoothd running?")?;
        let adapter = open_adapter(&session, options).await?;
        let agent = register_agent(&session, options.accept_pairing).await?;
        adapter.set_pairable(true).await.context("couldn't make the adapter pairable")?;

        let address = adapter.address().await?;
        let address_type = adapter.address_type().await?;
        let socket = Socket::<l2cap::Stream>::new_stream()?;
        socket.bind(SocketAddr::new(address, address_type, 0))?;
        socket.set_security(Security { level: SecurityLevel::Medium, key_size: 0 })?;
        socket.set_recv_mtu(RECV_MTU)?;
        let listener = socket.listen(4)?;
        let psm = listener.as_ref().local_addr()?.psm;
        debug!(psm, "listening on an L2CAP channel");

        let info = encode_info(psm).to_vec();
        let application = Application {
            services: vec![GattService {
                uuid: service_uuid(),
                primary: true,
                characteristics: vec![Characteristic {
                    uuid: info_uuid(),
                    read: Some(CharacteristicRead {
                        read: true,
                        encrypt_read: true,
                        fun: Box::new(move |_| {
                            let info = info.clone();
                            async move { Ok(info) }.boxed()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let application =
            adapter.serve_gatt_application(application).await.context("couldn't start the GATT service")?;

        let name = match &options.name {
            Some(name) => name.clone(),
            None => adapter.alias().await.unwrap_or_else(|_| "Saki".to_owned()),
        };
        let advertisement = Advertisement {
            service_uuids: HashSet::from([service_uuid()]).into_iter().collect(),
            discoverable: Some(true),
            local_name: Some(name.clone()),
            ..Default::default()
        };
        let advertisement = adapter.advertise(advertisement).await.context("couldn't start advertising")?;

        let accept_task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let client = Incoming { stream: Box::new(stream), peer: peer.addr.to_string() };
                        if incoming.send(client).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        warn!(%error, "couldn't accept an L2CAP connection");
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        });

        Ok(Service {
            name,
            _session: session,
            _agent: agent,
            _advertisement: advertisement,
            _application: application,
            accept_task,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

type DeviceEvents = Pin<Box<dyn futures::Stream<Item = AdapterEvent> + Send>>;

/// A running scan.
pub struct Scanner {
    session: Session,
    adapter: Adapter,
    agent: Arc<AgentHandle>,
    events: DeviceEvents,
    reported: HashSet<Address>,
}

impl Scanner {
    pub async fn start(options: &Options) -> Result<Self> {
        let session = Session::new().await.context("couldn't reach BlueZ. Is bluetoothd running?")?;
        let adapter = open_adapter(&session, options).await?;
        let agent = Arc::new(register_agent(&session, options.accept_pairing).await?);
        let filter = DiscoveryFilter {
            uuids: HashSet::from([service_uuid()]),
            transport: DiscoveryTransport::Le,
            ..Default::default()
        };
        adapter.set_discovery_filter(filter).await.context("couldn't set up the scan")?;
        let mut tries = SCAN_START_TRIES;
        let events = loop {
            match adapter.discover_devices_with_changes().await {
                // BlueZ takes a moment to wind down a scan that just ended, even one from another run.
                Err(error) if error.kind == bluer::ErrorKind::InProgress && tries > 1 => {
                    tries -= 1;
                    sleep(SCAN_START_RETRY).await;
                }
                result => break result.context("couldn't start scanning")?,
            }
        };
        Ok(Scanner { session, adapter, agent, events: Box::pin(events), reported: HashSet::new() })
    }

    /// Waits for the next server that hasn't been reported yet. BlueZ can announce a device before it
    /// knows what the device offers, so each device gets looked at again whenever it learns more.
    pub async fn next(&mut self) -> Result<Peer> {
        loop {
            let event = self.events.next().await.ok_or_else(|| anyhow!("the scan stopped"))?;
            let AdapterEvent::DeviceAdded(address) = event else {
                continue;
            };
            let device = self.adapter.device(address)?;
            let is_saki = device.uuids().await.ok().flatten().is_some_and(|uuids| uuids.contains(&service_uuid()));
            // BlueZ also lists devices it remembers, and those without a signal aren't here right now.
            let rssi = device.rssi().await.ok().flatten();
            if !is_saki || rssi.is_none() || !self.reported.insert(address) {
                continue;
            }
            return Ok(Peer { id: address.to_string(), name: device.name().await.ok().flatten(), rssi });
        }
    }

    pub async fn connect(self, peer: &Peer) -> Result<Link> {
        let Scanner { session, adapter, agent, events, .. } = self;
        // Scanning at the same time would slow connecting down, and BlueZ stops when this drops.
        drop(events);

        let address: Address = peer.id.parse().context("the peer's address is invalid")?;
        let device = adapter.device(address)?;
        let address_type = device.address_type().await.unwrap_or(AddressType::LePublic);

        if !device.is_connected().await? {
            timeout(CONNECT_TIMEOUT, device.connect())
                .await
                .map_err(|_| anyhow!("connecting to {} timed out", peer.label()))?
                .with_context(|| format!("couldn't connect to {}", peer.label()))?;
        }
        if !device.is_paired().await? {
            info!("pairing with {}. Confirm the code on both devices if asked", peer.label());
            timeout(PAIRING_TIMEOUT, device.pair())
                .await
                .map_err(|_| anyhow!("pairing with {} timed out", peer.label()))?
                .with_context(|| format!("couldn't pair with {}", peer.label()))?;
        }
        device.set_trusted(true).await?;

        let psm = timeout(RESOLVE_TIMEOUT, read_psm(&device))
            .await
            .map_err(|_| anyhow!("{} didn't offer the Saki service in time", peer.label()))??;
        debug!(psm, "the server's channel");

        let socket = Socket::<l2cap::Stream>::new_stream()?;
        socket.bind(SocketAddr::any_le())?;
        socket.set_security(Security { level: SecurityLevel::Medium, key_size: 0 })?;
        socket.set_recv_mtu(RECV_MTU)?;
        let stream = timeout(CONNECT_TIMEOUT, socket.connect(SocketAddr::new(address, address_type, psm)))
            .await
            .map_err(|_| anyhow!("opening the channel to {} timed out", peer.label()))?
            .context("couldn't open the L2CAP channel")?;

        Ok(Link { stream: Box::new(stream), keep_alive: Box::new((session, adapter, device, agent)) })
    }
}

/// Reads the info characteristic. Device discovery can report a device before its services are
/// known, so this waits for them.
async fn read_psm(device: &Device) -> Result<u16> {
    while !device.is_services_resolved().await? {
        sleep(Duration::from_millis(100)).await;
    }
    for service in device.services().await? {
        if service.uuid().await? != service_uuid() {
            continue;
        }
        for characteristic in service.characteristics().await? {
            if characteristic.uuid().await? == info_uuid() {
                let value = characteristic.read().await.context("couldn't read the server's info")?;
                let (version, psm) = decode_info(&value)?;
                if version != VERSION {
                    bail!("the server speaks protocol {version}, and this is {VERSION}. Update the older one");
                }
                return Ok(psm);
            }
        }
    }
    bail!("this device doesn't offer the Saki service")
}
