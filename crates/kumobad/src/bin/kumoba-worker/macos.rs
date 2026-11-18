use base64::{Engine, engine::general_purpose::STANDARD};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use kumoba_protocol::{AGENT_PORT, EGRESS_PORT, WorkerConfig};
use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_foundation::{NSArray, NSData, NSError, NSString, NSURL};
use objc2_virtualization::{
    VZDiskImageStorageDeviceAttachment, VZEntropyDeviceConfiguration,
    VZGraphicsDeviceConfiguration, VZKeyboardConfiguration, VZMACAddress, VZMacAuxiliaryStorage,
    VZMacGraphicsDeviceConfiguration, VZMacGraphicsDisplayConfiguration, VZMacHardwareModel,
    VZMacMachineIdentifier, VZMacOSBootLoader, VZMacPlatformConfiguration,
    VZNATNetworkDeviceAttachment, VZNetworkDeviceConfiguration, VZPointingDeviceConfiguration,
    VZSocketDeviceConfiguration, VZStorageDeviceConfiguration, VZUSBKeyboardConfiguration,
    VZUSBScreenCoordinatePointingDeviceConfiguration, VZVirtioBlockDeviceConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioNetworkDeviceConfiguration,
    VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtioSocketDeviceConfiguration,
    VZVirtioSocketListener, VZVirtioSocketListenerDelegate, VZVirtualMachine,
    VZVirtualMachineConfiguration, VZVirtualMachineDelegate,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
};

pub type Failure = Box<dyn std::error::Error>;
pub const LIMIT_EXIT: i32 = 75;
const VM_LIMIT_CODE: isize = 6;

#[derive(Serialize, Deserialize)]
pub struct Machine {
    pub hardware_model: String,
    pub machine_identifier: String,
    pub cpus: u8,
    pub memory_mib: u32,
}

impl Machine {
    pub fn load(directory: &Path) -> Result<Self, Failure> {
        Ok(serde_json::from_reader(File::open(
            directory.join("machine.json"),
        )?)?)
    }
}

pub struct Shared<T>(pub T);
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

pub enum Event {
    Stopped(Option<String>),
    OwnerGone,
}

pub struct VmError {
    pub message: String,
    pub code: isize,
}

impl From<VmError> for Failure {
    fn from(error: VmError) -> Self {
        error.message.into()
    }
}

pub fn ns_error(error: &NSError) -> VmError {
    VmError {
        message: format!(
            "{} ({} {})",
            error.localizedDescription(),
            error.domain(),
            error.code()
        ),
        code: error.code(),
    }
}

pub fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn data(encoded: &str) -> Result<Retained<NSData>, Failure> {
    Ok(NSData::with_bytes(&STANDARD.decode(encoded)?))
}

pub fn encode(data: &NSData) -> String {
    STANDARD.encode(data.to_vec())
}

pub fn configuration(
    machine: &Machine,
    directory: &Path,
    disk: &Path,
    network: Option<&VZMACAddress>,
) -> Result<Retained<VZVirtualMachineConfiguration>, Failure> {
    unsafe {
        let model = VZMacHardwareModel::initWithDataRepresentation(
            VZMacHardwareModel::alloc(),
            &*data(&machine.hardware_model)?,
        )
        .ok_or("the image has an invalid hardware model")?;
        if !model.isSupported() {
            return Err("this Mac cannot run the image's hardware model".into());
        }
        let identifier = VZMacMachineIdentifier::initWithDataRepresentation(
            VZMacMachineIdentifier::alloc(),
            &*data(&machine.machine_identifier)?,
        )
        .ok_or("the image has an invalid machine identifier")?;
        let platform = VZMacPlatformConfiguration::new();
        platform.setHardwareModel(&model);
        platform.setMachineIdentifier(&identifier);
        let auxiliary = VZMacAuxiliaryStorage::initWithURL(
            VZMacAuxiliaryStorage::alloc(),
            &url(&directory.join("aux.img")),
        );
        platform.setAuxiliaryStorage(Some(&auxiliary));

        let configuration = VZVirtualMachineConfiguration::new();
        configuration.setPlatform(&platform);
        configuration.setBootLoader(Some(&VZMacOSBootLoader::new()));
        configuration.setCPUCount(usize::from(machine.cpus));
        configuration.setMemorySize(u64::from(machine.memory_mib) * 1024 * 1024);

        let attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url(disk),
            false,
        )
        .map_err(|error| ns_error(&error))?;
        let block = VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            &attachment,
        );
        configuration.setStorageDevices(&NSArray::from_retained_slice(&[
            Retained::into_super(block) as Retained<VZStorageDeviceConfiguration>,
        ]));

        let display =
            VZMacGraphicsDisplayConfiguration::initWithWidthInPixels_heightInPixels_pixelsPerInch(
                VZMacGraphicsDisplayConfiguration::alloc(),
                1440,
                900,
                80,
            );
        let graphics = VZMacGraphicsDeviceConfiguration::new();
        graphics.setDisplays(&NSArray::from_retained_slice(&[display]));
        configuration
            .setGraphicsDevices(&NSArray::from_retained_slice(&[
                Retained::into_super(graphics) as Retained<VZGraphicsDeviceConfiguration>,
            ]));
        configuration.setKeyboards(&NSArray::from_retained_slice(&[Retained::into_super(
            VZUSBKeyboardConfiguration::new(),
        )
            as Retained<VZKeyboardConfiguration>]));
        configuration.setPointingDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZUSBScreenCoordinatePointingDeviceConfiguration::new(),
        )
            as Retained<VZPointingDeviceConfiguration>]));
        configuration.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )
            as Retained<VZEntropyDeviceConfiguration>]));
        configuration.setSocketDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioSocketDeviceConfiguration::new(),
        )
            as Retained<VZSocketDeviceConfiguration>]));
        if let Some(address) = network {
            let device = VZVirtioNetworkDeviceConfiguration::new();
            device.setAttachment(Some(&VZNATNetworkDeviceAttachment::new()));
            device.setMACAddress(address);
            configuration.setNetworkDevices(&NSArray::from_retained_slice(&[
                Retained::into_super(device) as Retained<VZNetworkDeviceConfiguration>,
            ]));
        }
        configuration
            .validateWithError()
            .map_err(|error| ns_error(&error))?;
        Ok(configuration)
    }
}

pub struct DelegateState {
    events: Mutex<mpsc::Sender<Event>>,
    egress: Option<PathBuf>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = DelegateState]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl VZVirtualMachineDelegate for Delegate {
        #[unsafe(method(guestDidStopVirtualMachine:))]
        fn guest_did_stop(&self, _machine: &VZVirtualMachine) {
            self.send(Event::Stopped(None));
        }

        #[unsafe(method(virtualMachine:didStopWithError:))]
        fn did_stop_with_error(&self, _machine: &VZVirtualMachine, error: &NSError) {
            self.send(Event::Stopped(Some(ns_error(error).message)));
        }
    }

    unsafe impl VZVirtioSocketListenerDelegate for Delegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:fromSocketDevice:))]
        fn should_accept(
            &self,
            _listener: &VZVirtioSocketListener,
            connection: &VZVirtioSocketConnection,
            _device: &VZVirtioSocketDevice,
        ) -> Bool {
            let Some(egress) = self.ivars().egress.clone() else {
                return Bool::NO;
            };
            let Ok(guest) = duplicate(unsafe { connection.fileDescriptor() }) else {
                return Bool::NO;
            };
            std::thread::spawn(move || {
                if let Ok(host) = UnixStream::connect(&egress) {
                    splice(guest, OwnedFd::from(host));
                }
            });
            Bool::YES
        }
    }
);

impl Delegate {
    pub fn new(events: mpsc::Sender<Event>, egress: Option<PathBuf>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateState {
            events: Mutex::new(events),
            egress,
        });
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, event: Event) {
        if let Ok(events) = self.ivars().events.lock() {
            let _ = events.send(event);
        }
    }
}

fn duplicate(fd: i32) -> std::io::Result<OwnedFd> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

pub fn splice(left: OwnedFd, right: OwnedFd) {
    let (Ok(left_writer), Ok(right_reader)) = (left.try_clone(), right.try_clone()) else {
        return;
    };
    let forward = std::thread::spawn(move || {
        let mut reader = File::from(left);
        let mut writer = File::from(right);
        let _ = std::io::copy(&mut reader, &mut writer);
        unsafe { libc::shutdown(writer.as_raw_fd(), libc::SHUT_WR) };
    });
    let mut reader = File::from(right_reader);
    let mut writer = File::from(left_writer);
    let _ = std::io::copy(&mut reader, &mut writer);
    unsafe { libc::shutdown(writer.as_raw_fd(), libc::SHUT_WR) };
    let _ = forward.join();
}

pub struct Vm {
    pub queue: DispatchRetained<DispatchQueue>,
    pub machine: Shared<Retained<VZVirtualMachine>>,
    _delegate: Shared<Retained<Delegate>>,
    _listener: Shared<Option<Retained<VZVirtioSocketListener>>>,
}

type Completion = block2::DynBlock<dyn Fn(*mut NSError)>;

impl Vm {
    pub fn new(
        configuration: Retained<VZVirtualMachineConfiguration>,
        events: mpsc::Sender<Event>,
        egress: Option<PathBuf>,
    ) -> Self {
        let queue = DispatchQueue::new("dev.kumoba.vm", None);
        let (sender, receiver) = mpsc::channel();
        let configuration = Shared(configuration);
        let on_queue = queue.clone();
        queue.exec_sync(move || {
            let configuration = configuration;
            let machine = unsafe {
                VZVirtualMachine::initWithConfiguration_queue(
                    VZVirtualMachine::alloc(),
                    &configuration.0,
                    &on_queue,
                )
            };
            let listening = egress.is_some();
            let delegate = Delegate::new(events, egress);
            unsafe { machine.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
            let listener = listening.then(|| unsafe {
                let listener = VZVirtioSocketListener::new();
                listener.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                if let Some(device) = socket_device(&machine) {
                    device.setSocketListener_forPort(&listener, EGRESS_PORT);
                }
                listener
            });
            let _ = sender.send(Shared((machine, delegate, listener)));
        });
        let Shared((machine, delegate, listener)) =
            receiver.recv().expect("virtual machine queue answered");
        Self {
            queue,
            machine: Shared(machine),
            _delegate: Shared(delegate),
            _listener: Shared(listener),
        }
    }

    pub fn call(
        &self,
        operation: impl FnOnce(&VZVirtualMachine, &Completion) + 'static,
    ) -> Result<(), VmError> {
        let (sender, receiver) = mpsc::channel();
        let machine = Shared(self.machine.0.clone());
        let operation = Shared(operation);
        self.queue.exec_async(move || {
            let machine = machine;
            let operation = operation;
            let block = RcBlock::new(move |error: *mut NSError| {
                let result = match unsafe { error.as_ref() } {
                    None => Ok(()),
                    Some(error) => Err(ns_error(error)),
                };
                let _ = sender.send(result);
            });
            (operation.0)(&machine.0, &block);
        });
        receiver.recv().unwrap_or_else(|_| {
            Err(VmError {
                message: "the virtual machine queue stopped".into(),
                code: 0,
            })
        })
    }

    pub fn on_queue<T: 'static>(&self, work: impl FnOnce(&VZVirtualMachine) -> T + 'static) -> T {
        let (sender, receiver) = mpsc::channel();
        let machine = Shared(self.machine.0.clone());
        let work = Shared(work);
        self.queue.exec_sync(move || {
            let machine = machine;
            let work = work;
            let _ = sender.send(Shared((work.0)(&machine.0)));
        });
        receiver.recv().expect("virtual machine queue answered").0
    }

    pub fn connect(&self, port: u32) -> Result<OwnedFd, String> {
        let (sender, receiver) = mpsc::channel();
        let machine = Shared(self.machine.0.clone());
        self.queue.exec_async(move || {
            let machine = machine;
            let Some(device) = (unsafe { socket_device(&machine.0) }) else {
                let _ = sender.send(Err("the virtual machine has no socket device".into()));
                return;
            };
            let block = RcBlock::new(
                move |connection: *mut VZVirtioSocketConnection, error: *mut NSError| {
                    let result = match (unsafe { connection.as_ref() }, unsafe { error.as_ref() }) {
                        (Some(connection), None) => {
                            duplicate(unsafe { connection.fileDescriptor() })
                                .map_err(|error| error.to_string())
                        }
                        (_, Some(error)) => Err(ns_error(error).message),
                        (None, None) => Err("no connection".into()),
                    };
                    let _ = sender.send(result);
                },
            );
            unsafe { device.connectToPort_completionHandler(port, &block) };
        });
        receiver
            .recv()
            .unwrap_or_else(|_| Err("the virtual machine queue stopped".into()))
    }

    pub fn start(&self) -> Result<(), VmError> {
        self.call(|machine, done| unsafe { machine.startWithCompletionHandler(done) })
    }

    pub fn stop(&self) -> Result<(), VmError> {
        self.call(|machine, done| unsafe { machine.stopWithCompletionHandler(done) })
    }
}

unsafe fn socket_device(machine: &VZVirtualMachine) -> Option<Retained<VZVirtioSocketDevice>> {
    let devices = unsafe { machine.socketDevices() };
    devices
        .firstObject()?
        .downcast::<VZVirtioSocketDevice>()
        .ok()
}

pub fn watch_owner(events: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        loop {
            match std::io::stdin().read(&mut byte) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                _ => break,
            }
        }
        let _ = events.send(Event::OwnerGone);
    });
}

pub fn serve_agent(vm: &std::sync::Arc<Vm>, socket: &Path) -> Result<(), Failure> {
    let listener = UnixListener::bind(socket)?;
    let vm = vm.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let vm = vm.clone();
            std::thread::spawn(move || {
                if let Ok(guest) = vm.connect(AGENT_PORT) {
                    splice(OwnedFd::from(stream), guest);
                }
            });
        }
    });
    Ok(())
}

pub fn run(config: &WorkerConfig, directory: &Path) -> Result<(), Failure> {
    let machine = Machine::load(directory)?;
    let (events, receiver) = mpsc::channel();
    let vm = Vm::new(
        configuration(&machine, directory, &config.disk, None)?,
        events.clone(),
        config.egress_socket.clone(),
    );
    match vm.start() {
        Ok(()) => {}
        Err(error) if error.code == VM_LIMIT_CODE => {
            eprintln!("kumoba-worker: {}", error.message);
            std::process::exit(LIMIT_EXIT);
        }
        Err(error) => return Err(error.into()),
    }
    let vm = std::sync::Arc::new(vm);
    serve_agent(&vm, &config.socket)?;
    watch_owner(events);
    match receiver.recv() {
        Ok(Event::Stopped(None)) => std::process::exit(0),
        Ok(Event::Stopped(Some(error))) => Err(error.into()),
        Ok(Event::OwnerGone) | Err(_) => {
            let _ = vm.stop();
            std::process::exit(125);
        }
    }
}
