use crate::macos::{Event, Failure, Shared, Vm, url};
use dispatch2::DispatchQueue;
use kumoba_protocol::screen::{
    DAMAGE, GRAB, GRABBED, HELLO, KEY, LEFT, MAX_SIDE, MIDDLE, POINTER, Parser, REFUSED, RIGHT,
    WHEEL, frame, pair, size,
};
use objc2::{
    AllocAnyThread, MainThreadMarker, MainThreadOnly,
    encode::{Encoding, RefEncode},
    msg_send,
    rc::{Allocated, Retained, autoreleasepool},
    runtime::{AnyClass, AnyObject, Sel},
    sel,
};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize};
use objc2_virtualization::{
    VZDiskImageStorageDeviceAttachment, VZEFIBootLoader, VZEFIVariableStore,
    VZEFIVariableStoreInitializationOptions, VZEntropyDeviceConfiguration,
    VZGenericPlatformConfiguration, VZGraphicsDeviceConfiguration, VZKeyboardConfiguration,
    VZPointingDeviceConfiguration, VZStorageDeviceConfiguration, VZUSBKeyboardConfiguration,
    VZUSBMassStorageDeviceConfiguration, VZUSBScreenCoordinatePointingDeviceConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioGraphicsDeviceConfiguration,
    VZVirtioGraphicsScanoutConfiguration, VZVirtualMachine, VZVirtualMachineConfiguration,
    VZVirtualMachineView,
};
use std::{
    cell::RefCell,
    ffi::{CString, c_void},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};

const MAX_VIEWERS: usize = 8;
const WATCH_EVERY: Duration = Duration::from_millis(8);
const WRITE_WAIT: Duration = Duration::from_secs(2);
const SURFACE_WAIT: Duration = Duration::from_millis(10);
const SURFACE_TRIES: usize = 50;
const WHEEL_LINES: i32 = -3;
const SCROLL_LINES: u32 = 1;
const KEY_DOWN: isize = 0;
const KEY_UP: isize = 1;
const READ_ONLY: u32 = 1;
const FD_BYTES: u32 = i32::BITS / 8;

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    fn IOSurfaceGetTypeID() -> usize;
    fn IOSurfaceLock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetWidth(surface: *mut c_void) -> usize;
    fn IOSurfaceGetHeight(surface: *mut c_void) -> usize;
    fn IOSurfaceGetBytesPerRow(surface: *mut c_void) -> usize;
    fn IOSurfaceGetBaseAddress(surface: *mut c_void) -> *const u8;
    fn IOSurfaceGetSeed(surface: *mut c_void) -> u32;
    fn IOSurfaceGetID(surface: *mut c_void) -> u32;
    fn IOSurfaceGetBytesPerElement(surface: *mut c_void) -> usize;
    fn IOSurfaceGetAllocSize(surface: *mut c_void) -> usize;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheels: u32,
        vertical: i32,
        horizontal: i32,
        third: i32,
    ) -> *mut CGEvent;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFRetain(value: *const c_void) -> *const c_void;
    fn CFRelease(value: *const c_void);
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopAddTimer(run_loop: *mut c_void, timer: *mut c_void, mode: *const c_void);
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: extern "C" fn(*mut c_void, *mut c_void),
        context: *mut c_void,
    ) -> *mut c_void;
}

extern "C" fn idle(_timer: *mut c_void, _context: *mut c_void) {}

pub fn run_main_loop() -> ! {
    const FAR: f64 = 1.0e9;
    unsafe {
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent() + FAR,
            FAR,
            0,
            0,
            idle,
            std::ptr::null_mut(),
        );
        CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopCommonModes);
        loop {
            CFRunLoopRun();
        }
    }
}

fn on_main<T: Send>(work: impl FnOnce(MainThreadMarker) -> T + Send) -> T {
    let mut result = None;
    DispatchQueue::main().exec_sync(|| {
        autoreleasepool(|_| {
            let marker = MainThreadMarker::new().expect("the main queue runs on the main thread");
            result = Some(work(marker));
        });
    });
    result.expect("the main queue ran the work")
}

struct Display {
    window: Retained<NSWindow>,
    view: Retained<VZVirtualMachineView>,
}

thread_local! {
    static DISPLAY: RefCell<Option<Display>> = const { RefCell::new(None) };
}

fn desktop_session() -> bool {
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return false;
    }
    unsafe { CFRelease(session) };
    true
}

fn show(machine: &VZVirtualMachine, width: u16, height: u16, marker: MainThreadMarker) {
    DISPLAY.with_borrow_mut(|display| {
        if display.is_some() {
            return;
        }
        let frame = NSRect::new(
            NSPoint::new(-20000.0, -20000.0),
            NSSize::new(f64::from(width), f64::from(height)),
        );
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        unsafe {
            let view =
                VZVirtualMachineView::initWithFrame(VZVirtualMachineView::alloc(marker), bounds);
            view.setVirtualMachine(Some(machine));
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(marker),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window.setIgnoresMouseEvents(true);
            window.setContentView(Some(&view));
            *display = Some(Display { window, view });
        }
    });
}

fn hide() {
    DISPLAY.with_borrow_mut(|display| {
        if let Some(display) = display.take() {
            unsafe { display.view.setVirtualMachine(None) };
            display.window.setContentView(None);
            display.window.close();
        }
    });
}

unsafe fn objects(list: *mut AnyObject) -> Vec<*mut AnyObject> {
    if list.is_null() {
        return Vec::new();
    }
    let count: usize = unsafe { msg_send![list, count] };
    (0..count)
        .map(|index| unsafe { msg_send![list, objectAtIndex: index] })
        .collect()
}

unsafe fn layer_surface(layer: *mut AnyObject) -> Option<*mut c_void> {
    if layer.is_null() {
        return None;
    }
    let contents: *mut AnyObject = unsafe { msg_send![layer, contents] };
    if !contents.is_null() && unsafe { CFGetTypeID(contents.cast()) == IOSurfaceGetTypeID() } {
        return Some(contents.cast());
    }
    let children: *mut AnyObject = unsafe { msg_send![layer, sublayers] };
    unsafe { objects(children) }
        .into_iter()
        .find_map(|child| unsafe { layer_surface(child) })
}

unsafe fn view_surface(view: *mut AnyObject) -> Option<*mut c_void> {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if let Some(found) = unsafe { layer_surface(layer) } {
        return Some(found);
    }
    let children: *mut AnyObject = unsafe { msg_send![view, subviews] };
    unsafe { objects(children) }
        .into_iter()
        .find_map(|child| unsafe { view_surface(child) })
}

struct Surface(*mut c_void);

impl Surface {
    fn current() -> Option<Self> {
        DISPLAY.with_borrow(|display| {
            let display = display.as_ref()?;
            let view: *const VZVirtualMachineView = &raw const *display.view;
            let found = unsafe { view_surface(view.cast_mut().cast()) }?;
            unsafe { CFRetain(found) };
            Some(Self(found))
        })
    }

    fn stamp(&self) -> (u32, u32) {
        unsafe { (IOSurfaceGetID(self.0), IOSurfaceGetSeed(self.0)) }
    }

    fn size(&self) -> Option<(u16, u16)> {
        let (width, height) = unsafe { (IOSurfaceGetWidth(self.0), IOSurfaceGetHeight(self.0)) };
        let side = |value: usize| {
            u16::try_from(value)
                .ok()
                .filter(|side| (1..=MAX_SIDE).contains(side))
        };
        Some((side(width)?, side(height)?))
    }

    fn copy(&self, target: *mut u8) {
        unsafe {
            let (width, height) = (IOSurfaceGetWidth(self.0), IOSurfaceGetHeight(self.0));
            if IOSurfaceLock(self.0, READ_ONLY, std::ptr::null_mut()) != 0 {
                return;
            }
            let (base, stride, row) = (
                IOSurfaceGetBaseAddress(self.0),
                IOSurfaceGetBytesPerRow(self.0),
                width * 4,
            );
            let whole = IOSurfaceGetBytesPerElement(self.0) == 4
                && stride >= row
                && IOSurfaceGetAllocSize(self.0) >= stride * height;
            if !base.is_null() && whole {
                for line in 0..height {
                    std::ptr::copy_nonoverlapping(
                        base.add(line * stride),
                        target.add(line * row),
                        row,
                    );
                }
            }
            IOSurfaceUnlock(self.0, READ_ONLY, std::ptr::null_mut());
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

fn stamp() -> Option<(u32, u32)> {
    Surface::current().map(|surface| surface.stamp())
}

fn settled() -> Option<(u16, u16)> {
    for _ in 0..SURFACE_TRIES {
        let size = on_main(|_| Surface::current().and_then(|surface| surface.size()));
        if size.is_some() {
            return size;
        }
        std::thread::sleep(SURFACE_WAIT);
    }
    None
}

struct Memory {
    fd: OwnedFd,
    start: *mut u8,
    bytes: usize,
    width: u16,
    height: u16,
}

unsafe impl Send for Memory {}
unsafe impl Sync for Memory {}

static MEMORIES: AtomicU32 = AtomicU32::new(0);

impl Memory {
    fn new(width: u16, height: u16) -> std::io::Result<Self> {
        let bytes = usize::from(width) * usize::from(height) * 4;
        let name = CString::new(format!(
            "/kumoba-{}-{}",
            std::process::id(),
            MEMORIES.fetch_add(1, Ordering::Relaxed)
        ))?;
        let raw = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        unsafe { libc::shm_unlink(name.as_ptr()) };
        let length = libc::off_t::try_from(bytes).map_err(std::io::Error::other)?;
        if unsafe { libc::ftruncate(fd.as_raw_fd(), length) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let start = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if start == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            start: start.cast(),
            bytes,
            width,
            height,
        })
    }

    fn fill(&self) -> (u16, u16) {
        let own = (self.width, self.height);
        let Some(surface) = Surface::current() else {
            return own;
        };
        let Some(size) = surface.size() else {
            return (0, 0);
        };
        if size == own {
            surface.copy(self.start);
        }
        size
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.start.cast(), self.bytes) };
    }
}

fn greet(socket: &UnixStream, memory: &Memory) -> std::io::Result<()> {
    let message = frame(HELLO, &size(memory.width, memory.height));
    let mut part = libc::iovec {
        iov_base: message.as_ptr().cast_mut().cast(),
        iov_len: message.len(),
    };
    let mut control = [0u64; 4];
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &raw mut part;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = unsafe { libc::CMSG_SPACE(FD_BYTES) } as _;
    unsafe {
        let entry = libc::CMSG_FIRSTHDR(&raw const header);
        (*entry).cmsg_level = libc::SOL_SOCKET;
        (*entry).cmsg_type = libc::SCM_RIGHTS;
        (*entry).cmsg_len = libc::CMSG_LEN(FD_BYTES) as _;
        libc::CMSG_DATA(entry)
            .cast::<i32>()
            .write_unaligned(memory.fd.as_raw_fd());
        if libc::sendmsg(socket.as_raw_fd(), &raw const header, 0) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

unsafe fn device(machine: &VZVirtualMachine, list: Sel, send: Sel) -> Option<*mut AnyObject> {
    let responds: bool = unsafe { msg_send![machine, respondsToSelector: list] };
    if !responds {
        return None;
    }
    let devices: *mut AnyObject = unsafe { msg_send![machine, performSelector: list] };
    unsafe { objects(devices) }.into_iter().find(|device| {
        let able: bool = unsafe { msg_send![*device, respondsToSelector: send] };
        able
    })
}

fn blank(name: &std::ffi::CStr, init: Sel) -> Option<Allocated<AnyObject>> {
    let class = AnyClass::get(name)?;
    class
        .instance_method(init)
        .map(|_| unsafe { msg_send![class, alloc] })
}

fn key(machine: &VZVirtualMachine, code: u16, down: bool) {
    let send = sel!(sendKeyEvents:);
    let init = sel!(initWithType:keyCode:);
    unsafe {
        let Some(keyboard) = device(machine, sel!(_keyboards), send) else {
            return;
        };
        let Some(blank) = blank(c"_VZKeyEvent", init) else {
            return;
        };
        let kind = if down { KEY_DOWN } else { KEY_UP };
        let event: Option<Retained<AnyObject>> =
            msg_send![blank, initWithType: kind, keyCode: code];
        if let Some(event) = event {
            let events = NSArray::from_retained_slice(&[event]);
            let () = msg_send![keyboard, sendKeyEvents: &*events];
        }
    }
}

fn point(machine: &VZVirtualMachine, x: f64, y: f64, buttons: u8) {
    let send = sel!(sendPointerEvents:);
    let init = sel!(initWithLocation:pressedButtons:);
    unsafe {
        let Some(pointer) = device(machine, sel!(_pointingDevices), send) else {
            return;
        };
        let Some(blank) = blank(c"_VZScreenCoordinatePointerEvent", init) else {
            return;
        };
        let mut pressed = 0isize;
        for (bit, value) in [(LEFT, 1), (RIGHT, 2), (MIDDLE, 4)] {
            if buttons & bit != 0 {
                pressed |= value;
            }
        }
        let event: Option<Retained<AnyObject>> =
            msg_send![blank, initWithLocation: NSPoint::new(x, y), pressedButtons: pressed];
        if let Some(event) = event {
            let events = NSArray::from_retained_slice(&[event]);
            let () = msg_send![pointer, sendPointerEvents: &*events];
        }
    }
}

#[repr(C)]
struct CGEvent {
    opaque: [u8; 0],
}

unsafe impl RefEncode for CGEvent {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
}

fn wheel(machine: &VZVirtualMachine, dx: i32, dy: i32) {
    let send = sel!(sendScrollWheelEvents:);
    let init = sel!(initWithEvent:);
    unsafe {
        let Some(pointer) = device(machine, sel!(_pointingDevices), send) else {
            return;
        };
        let (Some(blank), Some(events)) = (
            blank(c"_VZScrollWheelEvent", init),
            AnyClass::get(c"NSEvent"),
        ) else {
            return;
        };
        let turned = CGEventCreateScrollWheelEvent2(std::ptr::null(), SCROLL_LINES, 2, dy, dx, 0);
        if turned.is_null() {
            return;
        }
        let host: Option<Retained<AnyObject>> = msg_send![events, eventWithCGEvent: turned];
        CFRelease(turned.cast());
        let Some(host) = host else {
            return;
        };
        let event: Option<Retained<AnyObject>> = msg_send![blank, initWithEvent: &*host];
        if let Some(event) = event {
            let events = NSArray::from_retained_slice(&[event]);
            let () = msg_send![pointer, sendScrollWheelEvents: &*events];
        }
    }
}

fn side(value: f64) -> Option<u16> {
    let whole = value.fract() == 0.0 && (1.0..=f64::from(MAX_SIDE)).contains(&value);
    whole.then(|| unsafe { value.to_int_unchecked() })
}

fn display_size(vm: &Vm) -> Option<(u16, u16)> {
    vm.on_queue(|machine| unsafe {
        let display = machine
            .graphicsDevices()
            .firstObject()?
            .displays()
            .firstObject()?;
        let size = display.sizeInPixels();
        Some((side(size.width)?, side(size.height)?))
    })
}

struct Viewer {
    writer: Mutex<UnixStream>,
    told: AtomicBool,
}

impl Viewer {
    fn write(&self, message: &[u8]) -> std::io::Result<()> {
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_all(message)
    }
}

#[derive(Default)]
struct Viewers {
    open: Vec<Arc<Viewer>>,
    watching: bool,
}

static VIEWERS: Mutex<Viewers> = Mutex::new(Viewers {
    open: Vec::new(),
    watching: false,
});

fn viewers() -> std::sync::MutexGuard<'static, Viewers> {
    VIEWERS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn watch() {
    let mut last = None;
    loop {
        std::thread::sleep(WATCH_EVERY);
        let seen = on_main(|_| stamp());
        let mut viewers = viewers();
        if viewers.open.is_empty() {
            viewers.watching = false;
            on_main(|_| hide());
            return;
        }
        if seen == last {
            continue;
        }
        last = seen;
        for viewer in &viewers.open {
            if !viewer.told.swap(true, Ordering::AcqRel) {
                let _ = viewer.write(&frame(DAMAGE, &[]));
            }
        }
    }
}

fn join(vm: &Vm, viewer: &Arc<Viewer>, width: u16, height: u16) -> bool {
    let mut viewers = viewers();
    if viewers.open.len() >= MAX_VIEWERS {
        return false;
    }
    let machine = Shared(vm.machine.0.clone());
    on_main(move |marker| {
        let machine = machine;
        show(&machine.0, width, height, marker);
    });
    viewers.open.push(viewer.clone());
    if !viewers.watching {
        viewers.watching = true;
        std::thread::spawn(watch);
    }
    true
}

fn leave(viewer: &Arc<Viewer>) {
    viewers().open.retain(|open| !Arc::ptr_eq(open, viewer));
}

fn admit(vm: &Arc<Vm>, viewer: &Arc<Viewer>) -> Result<Memory, String> {
    if !desktop_session() {
        return Err("the live view needs kumobad to run inside a logged-in desktop session".into());
    }
    let shown = display_size(vm).ok_or("the sandbox has no display")?;
    if !join(vm, viewer, shown.0, shown.1) {
        return Err("too many live views are open on this sandbox".into());
    }
    let (width, height) = settled().unwrap_or(shown);
    Memory::new(width, height).map_err(|error| {
        leave(viewer);
        error.to_string()
    })
}

fn attend(vm: &Arc<Vm>, mut stream: UnixStream) -> Result<(), Failure> {
    stream.set_write_timeout(Some(WRITE_WAIT))?;
    let viewer = Arc::new(Viewer {
        writer: Mutex::new(stream.try_clone()?),
        told: AtomicBool::new(true),
    });
    let memory = match admit(vm, &viewer) {
        Ok(memory) => memory,
        Err(reason) => {
            let _ = viewer.write(&frame(REFUSED, reason.as_bytes()));
            return Err(reason.into());
        }
    };
    let result = converse(vm, &mut stream, &viewer, &memory);
    leave(&viewer);
    result
}

fn converse(
    vm: &Arc<Vm>,
    stream: &mut UnixStream,
    viewer: &Viewer,
    memory: &Memory,
) -> Result<(), Failure> {
    greet(stream, memory)?;
    let mut parser = Parser::default();
    let mut chunk = [0u8; 1024];
    let (width, height) = (f64::from(memory.width), f64::from(memory.height));
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Ok(());
        }
        parser.push(&chunk[..count]);
        while let Some((kind, payload)) = parser.take() {
            match (kind, payload.as_slice()) {
                (GRAB, _) => {
                    viewer.told.store(false, Ordering::Release);
                    let (width, height) = on_main(|_| memory.fill());
                    viewer.write(&frame(GRABBED, &size(width, height)))?;
                }
                (KEY, &[low, high, down]) => {
                    let code = u16::from_le_bytes([low, high]);
                    send(vm, move |machine| key(machine, code, down != 0));
                }
                (POINTER, &[_, _, _, _, buttons]) => {
                    let Some((x, y)) = pair(&payload) else {
                        continue;
                    };
                    let (x, y) = ((f64::from(x) + 0.5) / width, (f64::from(y) + 0.5) / height);
                    send(vm, move |machine| {
                        point(machine, x.min(1.0), y.min(1.0), buttons);
                    });
                }
                (WHEEL, &[x_low, x_high, y_low, y_high]) => {
                    let (dx, dy) = (
                        i32::from(i16::from_le_bytes([x_low, x_high])) * WHEEL_LINES,
                        i32::from(i16::from_le_bytes([y_low, y_high])) * WHEEL_LINES,
                    );
                    send(vm, move |machine| wheel(machine, dx, dy));
                }
                _ => {}
            }
        }
    }
}

fn send(vm: &Vm, work: impl FnOnce(&VZVirtualMachine) + Send + 'static) {
    let machine = Shared(vm.machine.0.clone());
    vm.queue.exec_async(move || {
        let machine = machine;
        autoreleasepool(|_| work(&machine.0));
    });
}

pub fn serve(vm: &Arc<Vm>, socket: &Path) -> Result<(), Failure> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let vm = vm.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let vm = vm.clone();
            std::thread::spawn(move || {
                if let Err(error) = attend(&vm, stream) {
                    eprintln!("kumoba-worker: live view ended: {error}");
                }
            });
        }
    });
    Ok(())
}

fn option(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).cloned()
}

pub fn check(args: &[String]) -> Result<(), Failure> {
    let usage = "usage: kumoba-worker screen-check --disk IMAGE --socket PATH";
    let disk = option(args, "--disk").ok_or(usage)?;
    let socket = option(args, "--socket").ok_or(usage)?;
    let store = format!("{socket}.efi");
    let _ = std::fs::remove_file(&store);
    let configuration = unsafe {
        let configuration = VZVirtualMachineConfiguration::new();
        configuration.setCPUCount(2);
        configuration.setMemorySize(1 << 30);
        let loader = VZEFIBootLoader::new();
        let variables = VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
            VZEFIVariableStore::alloc(),
            &url(Path::new(&store)),
            VZEFIVariableStoreInitializationOptions::empty(),
        )
        .map_err(|error| crate::macos::ns_error(&error).message)?;
        loader.setVariableStore(Some(&variables));
        configuration.setBootLoader(Some(&loader));
        configuration.setPlatform(&VZGenericPlatformConfiguration::new());
        let scanout = VZVirtioGraphicsScanoutConfiguration::initWithWidthInPixels_heightInPixels(
            VZVirtioGraphicsScanoutConfiguration::alloc(),
            1280,
            800,
        );
        let graphics = VZVirtioGraphicsDeviceConfiguration::new();
        graphics.setScanouts(&NSArray::from_retained_slice(&[scanout]));
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
        let attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &url(Path::new(&disk)),
            true,
        )
        .map_err(|error| crate::macos::ns_error(&error).message)?;
        let storage = VZUSBMassStorageDeviceConfiguration::initWithAttachment(
            VZUSBMassStorageDeviceConfiguration::alloc(),
            &attachment,
        );
        configuration
            .setStorageDevices(&NSArray::from_retained_slice(&[
                Retained::into_super(storage) as Retained<VZStorageDeviceConfiguration>,
            ]));
        configuration.setEntropyDevices(&NSArray::from_retained_slice(&[Retained::into_super(
            VZVirtioEntropyDeviceConfiguration::new(),
        )
            as Retained<VZEntropyDeviceConfiguration>]));
        configuration
            .validateWithError()
            .map_err(|error| crate::macos::ns_error(&error).message)?;
        configuration
    };
    let (events, receiver) = mpsc::channel();
    let vm = Arc::new(Vm::new(configuration, events, None));
    vm.start().map_err(|error| error.message)?;
    serve(&vm, Path::new(&socket))?;
    println!("screen-check: sharing the screen at {socket}");
    std::thread::spawn(move || {
        let code = match receiver.recv() {
            Ok(Event::Stopped(None)) => 0,
            _ => 1,
        };
        std::process::exit(code);
    });
    run_main_loop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_sides_a_screen_can_have_are_accepted() {
        assert_eq!(side(1280.0), Some(1280));
        assert_eq!(side(f64::from(MAX_SIDE)), Some(MAX_SIDE));
        for refused in [
            0.0,
            -4.0,
            800.5,
            f64::from(MAX_SIDE) + 1.0,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert_eq!(side(refused), None, "{refused} was accepted");
        }
    }

    #[test]
    fn shared_memory_holds_a_whole_screen_and_starts_blank() {
        let memory = Memory::new(64, 32).unwrap();
        assert_eq!(memory.bytes, 64 * 32 * 4);
        let pixels = unsafe { std::slice::from_raw_parts(memory.start, memory.bytes) };
        assert!(pixels.iter().all(|byte| *byte == 0));
    }
}
