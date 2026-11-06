//! Bluetooth on macOS, through Core Bluetooth.
//!
//! Core Bluetooth calls back on a dispatch queue, where the delegates below walk through the steps
//! of bringing a link up and report back over channels. Once a channel is open it's a pair of
//! NSStreams, which need a run loop, so each channel gets a thread running one.

use super::{Incoming, Link, Options, Peer};
use anyhow::{Result, anyhow, bail};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send, sel};
use objc2_core_bluetooth::{
    CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey, CBAttributePermissions, CBCentral,
    CBCentralManager, CBCentralManagerDelegate, CBCentralManagerScanOptionAllowDuplicatesKey, CBCharacteristic,
    CBCharacteristicProperties, CBL2CAPChannel, CBL2CAPPSM, CBManager, CBManagerAuthorization, CBManagerState,
    CBMutableCharacteristic, CBMutableService, CBPeripheral, CBPeripheralDelegate, CBPeripheralManager,
    CBPeripheralManagerConnectionLatency, CBPeripheralManagerDelegate, CBService, CBUUID,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSDictionary, NSError, NSInputStream, NSNumber, NSOutputStream,
    NSRunLoop, NSStream, NSStreamDelegate, NSStreamEvent, NSString, NSThread,
};
use saki_core::proto::{INFO_UUID, SERVICE_UUID, VERSION, decode_info, encode_info};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::CString;
use std::future::Future;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// Long enough for someone to answer the pairing dialog.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for someone to answer macOS's permission prompt.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(120);
const STREAM_BUFFER_BYTES: usize = 16 * 1024;
const STREAM_CHUNK_BYTES: usize = 4096;

/// Lets Core Bluetooth objects travel through Tokio on their way back to the queue or thread they
/// get used on.
struct Shared<T: ?Sized>(Retained<T>);
// SAFETY: these are only ever used on Core Bluetooth's queue or a channel's run loop thread. Core
// Bluetooth's own objects are documented as safe to pass between threads as long as that holds.
unsafe impl<T: ?Sized> Send for Shared<T> {}
unsafe impl<T: ?Sized> Sync for Shared<T> {}

impl<T: ?Sized> Shared<T> {
    /// Going through this makes a closure capture the wrapper rather than what's inside it.
    fn get(&self) -> &T {
        &self.0
    }
}

/// Makes macOS treat this process as its own app for Bluetooth permission.
///
/// macOS normally holds whichever app started a process responsible for what it does, and aborts
/// the process if that app never said why it wants Bluetooth. That would make Saki crash, with no
/// way to catch it, under any terminal that didn't happen to say so. Giving up that link makes macOS
/// look at the usage description built into this binary instead, and ask about "Saki" by name.
///
/// It can only be done while starting a process, so this restarts itself once, in place. The call
/// is private API, though Chromium and LLDB rely on it too, so it's looked up at runtime and it's
/// fine if it's gone.
pub fn take_responsibility() {
    const MARKER: &str = "SAKI_TOOK_RESPONSIBILITY";
    type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;

    if std::env::var_os(MARKER).is_some() {
        // SAFETY: this runs first thing in main, before there are any other threads.
        unsafe { std::env::remove_var(MARKER) };
        return;
    }
    let Ok(program) = std::env::current_exe().map(|path| path.into_os_string().into_vec()) else {
        return;
    };
    let Ok(program) = CString::new(program) else {
        return;
    };
    let arguments: Vec<CString> =
        std::env::args_os().filter_map(|argument| CString::new(argument.into_vec()).ok()).collect();
    let mut environment: Vec<CString> = std::env::vars_os()
        .filter_map(|(key, value)| {
            let mut pair = key.into_vec();
            pair.push(b'=');
            pair.extend(value.into_vec());
            CString::new(pair).ok()
        })
        .collect();
    environment.push(CString::new(format!("{MARKER}=1")).expect("the marker has no zero bytes"));

    let null_terminated = |strings: &[CString]| -> Vec<*mut libc::c_char> {
        strings.iter().map(|string| string.as_ptr().cast_mut()).chain(std::iter::once(std::ptr::null_mut())).collect()
    };
    let argv = null_terminated(&arguments);
    let envp = null_terminated(&environment);

    // SAFETY: the pointers all outlive the call, and on success posix_spawn doesn't return, since
    // POSIX_SPAWN_SETEXEC makes it replace this process. On failure, carrying on is still fine.
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_spawnattrs_setdisclaim".as_ptr());
        if symbol.is_null() {
            return;
        }
        let set_disclaim: SetDisclaim = std::mem::transmute(symbol);
        let mut attributes: libc::posix_spawnattr_t = std::ptr::null_mut();
        if libc::posix_spawnattr_init(&mut attributes) != 0 {
            return;
        }
        if libc::posix_spawnattr_setflags(&mut attributes, libc::POSIX_SPAWN_SETEXEC as libc::c_short) == 0
            && set_disclaim(&mut attributes, 1) == 0
        {
            libc::posix_spawn(
                std::ptr::null_mut(),
                program.as_ptr(),
                std::ptr::null(),
                &attributes,
                argv.as_ptr(),
                envp.as_ptr(),
            );
        }
        libc::posix_spawnattr_destroy(&mut attributes);
    }
}

fn uuid(value: &str) -> Retained<CBUUID> {
    unsafe { CBUUID::UUIDWithString(&NSString::from_str(value)) }
}

fn describe(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

/// What a state other than "powered on" means for the user, or `None` if it's worth waiting.
fn state_problem(state: CBManagerState) -> Option<&'static str> {
    match state {
        CBManagerState::PoweredOn | CBManagerState::Unknown | CBManagerState::Resetting => None,
        CBManagerState::PoweredOff => Some("Bluetooth is off. Turn it on in System Settings"),
        CBManagerState::Unauthorized => Some(
            "this app isn't allowed to use Bluetooth. Allow your terminal in System Settings > Privacy & Security > Bluetooth",
        ),
        CBManagerState::Unsupported => Some("this Mac doesn't support Bluetooth Low Energy"),
        _ => Some("Bluetooth is in an unknown state"),
    }
}

/// Waits for Core Bluetooth to come up, allowing extra time if macOS is still asking the user for
/// permission, which holds everything up until it's answered.
async fn wait_until_ready(ready: impl Future<Output = Result<()>>) -> Result<()> {
    let asking = unsafe { CBManager::authorization_class() } == CBManagerAuthorization::NotDetermined;
    let limit = if asking {
        info!("macOS is asking whether Saki may use Bluetooth. Please answer the prompt");
        PERMISSION_TIMEOUT
    } else {
        START_TIMEOUT
    };
    timeout(limit, ready).await.map_err(|_| {
        if asking {
            anyhow!("nobody answered macOS's Bluetooth permission prompt")
        } else {
            anyhow!("Bluetooth didn't become ready. Is it on?")
        }
    })?
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

enum CentralEvent {
    State(CBManagerState),
    Found { peer: Peer, peripheral: Shared<CBPeripheral> },
    Failed(String),
    Opened(Shared<CBL2CAPChannel>),
}

struct CentralIvars {
    events: mpsc::UnboundedSender<CentralEvent>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "SakiCentralDelegate"]
    #[ivars = CentralIvars]
    struct CentralDelegate;

    unsafe impl NSObjectProtocol for CentralDelegate {}

    unsafe impl CBCentralManagerDelegate for CentralDelegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn did_update_state(&self, central: &CBCentralManager) {
            self.send(CentralEvent::State(unsafe { central.state() }));
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn did_discover(
            &self,
            _central: &CBCentralManager,
            peripheral: &CBPeripheral,
            advertisement: &NSDictionary<NSString, AnyObject>,
            rssi: &NSNumber,
        ) {
            let advertised_name = advertisement
                .objectForKey(unsafe { CBAdvertisementDataLocalNameKey })
                .and_then(|name| name.downcast_ref::<NSString>().map(ToString::to_string));
            let name = advertised_name.or_else(|| unsafe { peripheral.name() }.map(|name| name.to_string()));
            let peer = Peer {
                id: unsafe { peripheral.identifier() }.UUIDString().to_string(),
                name,
                rssi: Some(rssi.as_i16()),
            };
            self.send(CentralEvent::Found { peer, peripheral: Shared(peripheral.retain()) });
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn did_connect(&self, _central: &CBCentralManager, peripheral: &CBPeripheral) {
            unsafe {
                peripheral.setDelegate(Some(ProtocolObject::from_ref(self)));
                peripheral.discoverServices(Some(&NSArray::from_retained_slice(&[uuid(SERVICE_UUID)])));
            }
        }

        #[unsafe(method(centralManager:didFailToConnectPeripheral:error:))]
        fn did_fail_to_connect(
            &self,
            _central: &CBCentralManager,
            _peripheral: &CBPeripheral,
            error: Option<&NSError>,
        ) {
            self.fail("couldn't connect", error);
        }

        #[unsafe(method(centralManager:didDisconnectPeripheral:error:))]
        fn did_disconnect(&self, _central: &CBCentralManager, _peripheral: &CBPeripheral, error: Option<&NSError>) {
            self.fail("disconnected", error);
        }
    }

    unsafe impl CBPeripheralDelegate for CentralDelegate {
        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn did_discover_services(&self, peripheral: &CBPeripheral, error: Option<&NSError>) {
            if error.is_some() {
                return self.fail("couldn't list the server's services", error);
            }
            let wanted = uuid(SERVICE_UUID);
            let services = unsafe { peripheral.services() }.unwrap_or_else(NSArray::new);
            let service = services.iter().find(|service| unsafe { service.UUID() } == wanted);
            match service {
                Some(service) => unsafe {
                    let characteristics = NSArray::from_retained_slice(&[uuid(INFO_UUID)]);
                    peripheral.discoverCharacteristics_forService(Some(&characteristics), &service);
                },
                None => self.fail("this device doesn't offer the Saki service", None),
            }
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn did_discover_characteristics(
            &self,
            peripheral: &CBPeripheral,
            service: &CBService,
            error: Option<&NSError>,
        ) {
            if error.is_some() {
                return self.fail("couldn't list the service's characteristics", error);
            }
            let wanted = uuid(INFO_UUID);
            let characteristics = unsafe { service.characteristics() }.unwrap_or_else(NSArray::new);
            match characteristics.iter().find(|characteristic| unsafe { characteristic.UUID() } == wanted) {
                // The characteristic needs an encrypted link, so this read is what starts pairing.
                Some(characteristic) => unsafe { peripheral.readValueForCharacteristic(&characteristic) },
                None => self.fail("the Saki service is missing its info characteristic", None),
            }
        }

        #[unsafe(method(peripheral:didUpdateValueForCharacteristic:error:))]
        fn did_update_value(
            &self,
            peripheral: &CBPeripheral,
            characteristic: &CBCharacteristic,
            error: Option<&NSError>,
        ) {
            if error.is_some() {
                return self.fail("couldn't read the server's info. Did pairing get refused?", error);
            }
            let value = unsafe { characteristic.value() }.map(|value| value.to_vec()).unwrap_or_default();
            match decode_info(&value) {
                Ok((version, _)) if version != VERSION => {
                    let message =
                        format!("the server speaks protocol {version}, and this is {VERSION}. Update the older one");
                    self.send(CentralEvent::Failed(message));
                }
                Ok((_, psm)) => {
                    debug!(psm, "the server's channel");
                    unsafe { peripheral.openL2CAPChannel(CBL2CAPPSM::from(psm)) };
                }
                Err(error) => self.send(CentralEvent::Failed(error.to_string())),
            }
        }

        #[unsafe(method(peripheral:didOpenL2CAPChannel:error:))]
        fn did_open_channel(
            &self,
            _peripheral: &CBPeripheral,
            channel: Option<&CBL2CAPChannel>,
            error: Option<&NSError>,
        ) {
            match channel {
                Some(channel) if error.is_none() => self.send(CentralEvent::Opened(Shared(channel.retain()))),
                _ => self.fail("couldn't open the L2CAP channel", error),
            }
        }
    }
);

impl CentralDelegate {
    fn create(events: mpsc::UnboundedSender<CentralEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(CentralIvars { events });
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, event: CentralEvent) {
        let _ = self.ivars().events.send(event);
    }

    fn fail(&self, what: &str, error: Option<&NSError>) {
        let message = match error {
            Some(error) => format!("{what}: {}", describe(error)),
            None => what.to_owned(),
        };
        self.send(CentralEvent::Failed(message));
    }
}

/// A running scan.
pub struct Scanner {
    queue: DispatchRetained<DispatchQueue>,
    manager: Shared<CBCentralManager>,
    delegate: Shared<CentralDelegate>,
    events: mpsc::UnboundedReceiver<CentralEvent>,
    found: HashMap<String, Shared<CBPeripheral>>,
    /// The servers already reported along with their names.
    named: HashSet<String>,
}

impl Scanner {
    pub async fn start(_options: &Options) -> Result<Self> {
        let queue = DispatchQueue::new("com.wonderhoylabs.saki.central", None);
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let delegate = CentralDelegate::create(events_tx);
        let manager = unsafe {
            CBCentralManager::initWithDelegate_queue(
                CBCentralManager::alloc(),
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            )
        };
        let wait = async {
            while let Some(event) = events.recv().await {
                if let CentralEvent::State(state) = event {
                    if state == CBManagerState::PoweredOn {
                        return Ok(());
                    }
                    if let Some(problem) = state_problem(state) {
                        bail!("{problem}");
                    }
                }
            }
            bail!("Core Bluetooth went away")
        };
        wait_until_ready(wait).await?;

        let manager = Shared(manager);
        let scanning = Shared(manager.0.clone());
        queue.exec_async(move || unsafe {
            let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
            // A server's name often only comes in a later report, and without this macOS only
            // gives one report per device.
            let allow = NSNumber::new_bool(true);
            let options: Retained<NSDictionary<NSString, AnyObject>> =
                NSDictionary::from_slices(&[CBCentralManagerScanOptionAllowDuplicatesKey], &[allow.as_ref()]);
            scanning.get().scanForPeripheralsWithServices_options(Some(&services), Some(&options));
        });
        Ok(Scanner { queue, manager, delegate: Shared(delegate), events, found: HashMap::new(), named: HashSet::new() })
    }

    /// Waits for the next server that hasn't been reported yet, or one whose name just came in.
    pub async fn next(&mut self) -> Result<Peer> {
        loop {
            match self.events.recv().await {
                Some(CentralEvent::Found { peer, peripheral }) => {
                    let is_new = self.found.insert(peer.id.clone(), peripheral).is_none();
                    let is_newly_named = peer.name.is_some() && self.named.insert(peer.id.clone());
                    if is_new || is_newly_named {
                        return Ok(peer);
                    }
                }
                Some(CentralEvent::State(state)) => {
                    if let Some(problem) = state_problem(state) {
                        bail!("{problem}");
                    }
                }
                Some(_) => {}
                None => bail!("the scan stopped"),
            }
        }
    }

    pub async fn connect(mut self, peer: &Peer) -> Result<Link> {
        let peripheral = self.found.remove(&peer.id).ok_or_else(|| anyhow!("{} hasn't been found", peer.label()))?;
        let peripheral = Arc::new(peripheral);

        let manager = Shared(self.manager.0.clone());
        let target = peripheral.clone();
        self.queue.exec_async(move || unsafe {
            manager.get().stopScan();
            manager.get().connectPeripheral_options(target.get(), None);
        });

        let events = &mut self.events;
        let wait = async {
            loop {
                match events.recv().await {
                    Some(CentralEvent::Opened(channel)) => return Ok(channel),
                    Some(CentralEvent::Failed(message)) => bail!("{message}"),
                    Some(CentralEvent::State(state)) => {
                        if let Some(problem) = state_problem(state) {
                            bail!("{problem}");
                        }
                    }
                    Some(CentralEvent::Found { .. }) => {}
                    None => bail!("Core Bluetooth went away"),
                }
            }
        };
        let result = timeout(CONNECT_TIMEOUT, wait).await;
        let channel = match result {
            Ok(Ok(channel)) => channel,
            Ok(Err(error)) => {
                self.disconnect(&peripheral);
                return Err(error);
            }
            Err(_) => {
                self.disconnect(&peripheral);
                bail!("connecting to {} timed out. Did the pairing prompt get answered?", peer.label());
            }
        };

        let stream = ChannelStream::open(channel);
        Ok(Link { stream: Box::new(stream), keep_alive: Box::new(CentralConnection { scanner: self, peripheral }) })
    }

    fn disconnect(&self, peripheral: &Arc<Shared<CBPeripheral>>) {
        let manager = Shared(self.manager.0.clone());
        let peripheral = peripheral.clone();
        self.queue.exec_async(move || unsafe {
            manager.get().cancelPeripheralConnection(peripheral.get());
        });
    }
}

/// Keeps a client's connection up. Dropping it disconnects.
struct CentralConnection {
    scanner: Scanner,
    peripheral: Arc<Shared<CBPeripheral>>,
}

impl Drop for CentralConnection {
    fn drop(&mut self) {
        self.scanner.disconnect(&self.peripheral);
        // The manager has to outlive the disconnect it was just asked to do.
        let _ = &self.scanner.delegate;
    }
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

enum ServerEvent {
    Advertising,
    Failed(String),
}

struct PeripheralIvars {
    name: String,
    events: mpsc::UnboundedSender<ServerEvent>,
    incoming: mpsc::Sender<Incoming>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "SakiPeripheralDelegate"]
    #[ivars = PeripheralIvars]
    struct PeripheralDelegate;

    unsafe impl NSObjectProtocol for PeripheralDelegate {}

    unsafe impl CBPeripheralManagerDelegate for PeripheralDelegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn did_update_state(&self, manager: &CBPeripheralManager) {
            let state = unsafe { manager.state() };
            if state == CBManagerState::PoweredOn {
                // This also runs when Bluetooth comes back on, which clears everything published.
                unsafe {
                    manager.removeAllServices();
                    manager.publishL2CAPChannelWithEncryption(true);
                }
            } else if let Some(problem) = state_problem(state) {
                warn!("{problem}");
                self.send(ServerEvent::Failed(problem.to_owned()));
            }
        }

        #[unsafe(method(peripheralManager:didPublishL2CAPChannel:error:))]
        fn did_publish_channel(&self, manager: &CBPeripheralManager, psm: CBL2CAPPSM, error: Option<&NSError>) {
            if let Some(error) = error {
                return self
                    .send(ServerEvent::Failed(format!("couldn't publish an L2CAP channel: {}", describe(error))));
            }
            debug!(psm, "listening on an L2CAP channel");
            unsafe {
                let info = NSData::with_bytes(&encode_info(psm as u16));
                let characteristic = CBMutableCharacteristic::initWithType_properties_value_permissions(
                    CBMutableCharacteristic::alloc(),
                    &uuid(INFO_UUID),
                    CBCharacteristicProperties::Read,
                    Some(&info),
                    // Reading this needs an encrypted link, so it's what makes clients pair.
                    CBAttributePermissions::ReadEncryptionRequired,
                );
                let service =
                    CBMutableService::initWithType_primary(CBMutableService::alloc(), &uuid(SERVICE_UUID), true);
                let characteristics = NSArray::from_retained_slice(&[Retained::into_super(characteristic)]);
                service.setCharacteristics(Some(&characteristics));
                manager.addService(&service);
            }
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn did_add_service(&self, manager: &CBPeripheralManager, _service: &CBService, error: Option<&NSError>) {
            if let Some(error) = error {
                return self.send(ServerEvent::Failed(format!("couldn't add the GATT service: {}", describe(error))));
            }
            unsafe {
                let name = NSString::from_str(&self.ivars().name);
                let services = NSArray::from_retained_slice(&[uuid(SERVICE_UUID)]);
                let advertisement: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
                    &[CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey],
                    &[name.as_ref(), services.as_ref()],
                );
                manager.startAdvertising(Some(&advertisement));
            }
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn did_start_advertising(&self, _manager: &CBPeripheralManager, error: Option<&NSError>) {
            match error {
                Some(error) => {
                    self.send(ServerEvent::Failed(format!("couldn't start advertising: {}", describe(error))))
                }
                None => self.send(ServerEvent::Advertising),
            }
        }

        #[unsafe(method(peripheralManager:didOpenL2CAPChannel:error:))]
        fn did_open_channel(
            &self,
            manager: &CBPeripheralManager,
            channel: Option<&CBL2CAPChannel>,
            error: Option<&NSError>,
        ) {
            let Some(channel) = channel.filter(|_| error.is_none()) else {
                warn!("a client couldn't open a channel: {}", error.map_or_else(String::new, describe));
                return;
            };
            let peer = unsafe { channel.peer() };
            let peer_id = peer
                .as_ref()
                .map_or_else(|| "unknown".to_owned(), |peer| unsafe { peer.identifier() }.UUIDString().to_string());
            if let Some(central) = peer.and_then(|peer| peer.downcast::<CBCentral>().ok()) {
                unsafe {
                    manager.setDesiredConnectionLatency_forCentral(CBPeripheralManagerConnectionLatency::Low, &central)
                };
            }
            let stream = ChannelStream::open(Shared(channel.retain()));
            let client = Incoming { stream: Box::new(stream), peer: peer_id };
            if self.ivars().incoming.try_send(client).is_err() {
                warn!("too many clients arrived at once, so one was turned away");
            }
        }
    }
);

impl PeripheralDelegate {
    fn create(
        name: String,
        events: mpsc::UnboundedSender<ServerEvent>,
        incoming: mpsc::Sender<Incoming>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(PeripheralIvars { name, events, incoming });
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, event: ServerEvent) {
        let _ = self.ivars().events.send(event);
    }
}

/// A running server. Dropping it stops advertising.
pub struct Service {
    name: String,
    queue: DispatchRetained<DispatchQueue>,
    manager: Shared<CBPeripheralManager>,
    _delegate: Shared<PeripheralDelegate>,
}

impl Service {
    pub async fn start(options: &Options, incoming: mpsc::Sender<Incoming>) -> Result<Self> {
        let name = options.name.clone().unwrap_or_else(computer_name);
        let queue = DispatchQueue::new("com.wonderhoylabs.saki.peripheral", None);
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let delegate = PeripheralDelegate::create(name.clone(), events_tx, incoming);
        let manager = unsafe {
            CBPeripheralManager::initWithDelegate_queue(
                CBPeripheralManager::alloc(),
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            )
        };

        let wait = async {
            match events.recv().await {
                Some(ServerEvent::Advertising) => Ok(()),
                Some(ServerEvent::Failed(message)) => Err(anyhow!("{message}")),
                None => Err(anyhow!("Core Bluetooth went away")),
            }
        };
        wait_until_ready(wait).await?;

        info!("this Mac can't accept first-time pairing as a server, so new clients have to pair the other way first");
        info!("to do that, share the client's network and run `saki probe` here. The README has the details");

        // Later failures, like Bluetooth being turned off, are only worth logging.
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let ServerEvent::Failed(message) = event {
                    warn!("{message}");
                }
            }
        });
        Ok(Service { name, queue, manager: Shared(manager), _delegate: Shared(delegate) })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let manager = Shared(self.manager.0.clone());
        self.queue.exec_async(move || unsafe {
            manager.get().stopAdvertising();
            manager.get().removeAllServices();
        });
    }
}

fn computer_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Saki".to_owned())
}

// ---------------------------------------------------------------------------------------------
// The byte stream
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Buffers {
    incoming: VecDeque<u8>,
    outgoing: VecDeque<u8>,
    read_waker: Option<Waker>,
    write_waker: Option<Waker>,
    /// The channel ended, and why if it wasn't a clean end.
    closed: Option<Option<String>>,
    stopping: bool,
}

struct StreamIvars {
    buffers: Arc<Mutex<Buffers>>,
    pump_scheduled: Arc<AtomicBool>,
    input: Retained<NSInputStream>,
    output: Retained<NSOutputStream>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "SakiStreamDelegate"]
    #[ivars = StreamIvars]
    struct StreamDelegate;

    impl StreamDelegate {
        /// Asked for from other threads, which is what wakes the run loop up.
        #[unsafe(method(pump:))]
        fn pump(&self, _unused: Option<&AnyObject>) {
            self.ivars().pump_scheduled.store(false, Ordering::Release);
            self.move_bytes();
        }
    }

    unsafe impl NSObjectProtocol for StreamDelegate {}

    unsafe impl NSStreamDelegate for StreamDelegate {
        #[unsafe(method(stream:handleEvent:))]
        fn stream_handle_event(&self, stream: &NSStream, event: NSStreamEvent) {
            if event.contains(NSStreamEvent::ErrorOccurred) {
                self.close(stream.streamError().map(|error| describe(&error)).or_else(|| Some("the channel failed".to_owned())));
            } else if event.contains(NSStreamEvent::EndEncountered) {
                self.close(None);
            }
            self.move_bytes();
        }
    }
);

impl StreamDelegate {
    fn create(ivars: StreamIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn close(&self, error: Option<String>) {
        let mut buffers = self.ivars().buffers.lock().unwrap();
        if buffers.closed.is_none() {
            buffers.closed = Some(error);
        }
        let wakers = [buffers.read_waker.take(), buffers.write_waker.take()];
        drop(buffers);
        wakers.into_iter().flatten().for_each(Waker::wake);
    }

    /// Moves whatever can move without blocking, in both directions.
    fn move_bytes(&self) {
        let ivars = self.ivars();
        let mut buffers = ivars.buffers.lock().unwrap();
        if buffers.stopping {
            return;
        }

        let mut chunk = [0u8; STREAM_CHUNK_BYTES];
        let pointer = NonNull::from(&mut chunk).cast::<u8>();
        let mut did_read = false;
        while ivars.input.hasBytesAvailable() && buffers.incoming.len() < STREAM_BUFFER_BYTES {
            let len = unsafe { ivars.input.read_maxLength(pointer, chunk.len()) };
            if len <= 0 {
                break;
            }
            buffers.incoming.extend(&chunk[..len as usize]);
            did_read = true;
        }

        let mut did_write = false;
        while !buffers.outgoing.is_empty() && ivars.output.hasSpaceAvailable() {
            let (front, _) = buffers.outgoing.as_slices();
            let pointer = NonNull::from(front).cast::<u8>();
            let len = unsafe { ivars.output.write_maxLength(pointer, front.len()) };
            if len <= 0 {
                break;
            }
            buffers.outgoing.drain(..len as usize);
            did_write = true;
        }

        let read_waker = if did_read { buffers.read_waker.take() } else { None };
        let write_waker = if did_write { buffers.write_waker.take() } else { None };
        drop(buffers);
        [read_waker, write_waker].into_iter().flatten().for_each(Waker::wake);
    }
}

/// An open L2CAP channel as a Tokio stream. Dropping it closes the channel.
struct ChannelStream {
    buffers: Arc<Mutex<Buffers>>,
    pump_scheduled: Arc<AtomicBool>,
    delegate: Shared<StreamDelegate>,
    thread: Shared<NSThread>,
}

impl ChannelStream {
    fn open(channel: Shared<CBL2CAPChannel>) -> Self {
        let buffers = Arc::new(Mutex::new(Buffers::default()));
        let pump_scheduled = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let thread_buffers = buffers.clone();
        let thread_pump_scheduled = pump_scheduled.clone();
        std::thread::Builder::new()
            .name("saki-l2cap".to_owned())
            .spawn(move || run_channel(channel, thread_buffers, thread_pump_scheduled, ready_tx))
            .expect("couldn't start a thread for the channel");
        let (delegate, thread) = ready_rx.recv().expect("the channel's thread stopped before it started");

        ChannelStream { buffers, pump_scheduled, delegate, thread }
    }

    /// Gets the run loop to look at the buffers.
    fn schedule_pump(&self) {
        if self.pump_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let none: Option<&AnyObject> = None;
        let _: () = unsafe {
            msg_send![
                &*self.delegate.0,
                performSelector: sel!(pump:),
                onThread: &*self.thread.0,
                withObject: none,
                waitUntilDone: false,
            ]
        };
    }
}

fn run_channel(
    channel: Shared<CBL2CAPChannel>,
    buffers: Arc<Mutex<Buffers>>,
    pump_scheduled: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<(Shared<StreamDelegate>, Shared<NSThread>)>,
) {
    let run_loop = NSRunLoop::currentRunLoop();
    let (Some(input), Some(output)) = (unsafe { channel.0.inputStream() }, unsafe { channel.0.outputStream() }) else {
        buffers.lock().unwrap().closed = Some(Some("the channel has no streams".to_owned()));
        let delegate = StreamDelegate::create(StreamIvars {
            buffers,
            pump_scheduled,
            input: NSInputStream::new(),
            output: NSOutputStream::new(),
        });
        let _ = ready.send((Shared(delegate), Shared(NSThread::currentThread())));
        return;
    };

    let delegate = StreamDelegate::create(StreamIvars {
        buffers: buffers.clone(),
        pump_scheduled,
        input: input.clone(),
        output: output.clone(),
    });
    unsafe {
        let mode = NSDefaultRunLoopMode;
        input.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        output.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        input.scheduleInRunLoop_forMode(&run_loop, mode);
        output.scheduleInRunLoop_forMode(&run_loop, mode);
    }
    input.open();
    output.open();
    let _ = ready.send((Shared(delegate.clone()), Shared(NSThread::currentThread())));

    loop {
        if buffers.lock().unwrap().stopping {
            break;
        }
        autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(30.0);
            unsafe { run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &until) };
        });
    }

    input.close();
    output.close();
    unsafe {
        input.removeFromRunLoop_forMode(&run_loop, NSDefaultRunLoopMode);
        output.removeFromRunLoop_forMode(&run_loop, NSDefaultRunLoopMode);
        input.setDelegate(None);
        output.setDelegate(None);
    }
    // The channel stays retained up to here, because closing it early would end the streams.
    drop(channel);
}

impl AsyncRead for ChannelStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut buffers = self.buffers.lock().unwrap();
        if !buffers.incoming.is_empty() {
            let was_full = buffers.incoming.len() >= STREAM_BUFFER_BYTES;
            let (front, _) = buffers.incoming.as_slices();
            let len = front.len().min(buf.remaining());
            buf.put_slice(&front[..len]);
            buffers.incoming.drain(..len);
            drop(buffers);
            if was_full {
                self.schedule_pump();
            }
            return Poll::Ready(Ok(()));
        }
        match &buffers.closed {
            Some(Some(error)) => Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, error.clone()))),
            Some(None) => Poll::Ready(Ok(())),
            None => {
                buffers.read_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl AsyncWrite for ChannelStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let mut buffers = self.buffers.lock().unwrap();
        if buffers.closed.is_some() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let room = STREAM_BUFFER_BYTES.saturating_sub(buffers.outgoing.len());
        if room == 0 {
            buffers.write_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let len = room.min(data.len());
        buffers.outgoing.extend(&data[..len]);
        drop(buffers);
        self.schedule_pump();
        Poll::Ready(Ok(len))
    }

    /// Waits for the radio to take everything. Tying the session's writes to this keeps packets
    /// queued where they can still be dropped, rather than here where they can't.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        let mut buffers = self.buffers.lock().unwrap();
        if buffers.closed.is_some() {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if buffers.outgoing.is_empty() {
            return Poll::Ready(Ok(()));
        }
        buffers.write_waker = Some(cx.waker().clone());
        Poll::Pending
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for ChannelStream {
    fn drop(&mut self) {
        self.buffers.lock().unwrap().stopping = true;
        self.schedule_pump();
    }
}
