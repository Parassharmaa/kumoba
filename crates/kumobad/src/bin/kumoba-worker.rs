#[path = "kumoba-worker/confinement.rs"]
mod confinement;
use libloading::Library;
use kumoba_protocol::{AGENT_PORT, EGRESS_PORT, VERSION, WorkerConfig};
use std::{
    ffi::{CString, c_char},
    io::{BufRead, Read},
    os::unix::ffi::OsStrExt,
};

type Failure = Box<dyn std::error::Error>;

fn main() {
    if let Err(error) = run() {
        eprintln!("kumoba-worker: {error}");
        if cfg!(target_os = "macos") {
            eprintln!(
                "kumoba-worker: on macOS this binary needs the com.apple.security.hypervisor entitlement; sign it as described at https://github.com/Parassharmaa/kumoba#install"
            );
        }
        std::process::exit(125);
    }
}

fn check(call: &str, result: i32) -> Result<u32, Failure> {
    if result < 0 {
        Err(format!("{call}: {}", std::io::Error::from_raw_os_error(-result)).into())
    } else {
        Ok(result as u32)
    }
}

fn run() -> Result<(), Failure> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(64 * 1024)
        .read_line(&mut line)?;
    let config: WorkerConfig = serde_json::from_str(&line)?;
    if config.version != VERSION
        || !(1..=8).contains(&config.vcpus)
        || !(256..=16384).contains(&config.memory_mib)
        || config.lock_fd < 3
    {
        return Err("invalid worker configuration".into());
    }
    let library = unsafe { Library::new(&config.library)? };
    let _firmware = unsafe { Library::new(&config.firmware)? };
    if !config.mounts.is_empty() {
        confinement::apply(&config)?;
    }
    let disk = CString::new(config.disk.as_os_str().as_bytes())?;
    let socket = CString::new(config.socket.as_os_str().as_bytes())?;
    let console = CString::new(config.console.as_os_str().as_bytes())?;
    unsafe {
        let log =
            library.get::<unsafe extern "C" fn(i32, u32, u32, u32) -> i32>(b"krun_init_log\0")?;
        check("log", log(-1, 1, 2, 1))?;
        let create = library.get::<unsafe extern "C" fn() -> i32>(b"krun_create_ctx\0")?;
        let ctx = check("create", create())?;
        let vm =
            library.get::<unsafe extern "C" fn(u32, u8, u32) -> i32>(b"krun_set_vm_config\0")?;
        check("resources", vm(ctx, config.vcpus, config.memory_mib))?;
        let disk_fn = library.get::<unsafe extern "C" fn(
            u32,
            *const c_char,
            *const c_char,
            u32,
            bool,
            bool,
            u32,
        ) -> i32>(b"krun_add_disk3\0")?;
        check(
            "disk",
            disk_fn(ctx, c"root".as_ptr(), disk.as_ptr(), 0, false, false, 2),
        )?;
        let root = library.get::<unsafe extern "C" fn(
            u32,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> i32>(b"krun_set_root_disk_remount\0")?;
        check(
            "root",
            root(
                ctx,
                c"/dev/vda".as_ptr(),
                c"ext4".as_ptr(),
                std::ptr::null(),
            ),
        )?;
        if !config.mounts.is_empty() {
            let mount = library.get::<unsafe extern "C" fn(
                u32,
                *const c_char,
                *const c_char,
                u64,
                bool,
            ) -> i32>(b"krun_add_virtiofs3\0")?;
            for (index, share) in config.mounts.iter().enumerate() {
                let tag = CString::new(format!("kumoba-{index}"))?;
                let path = CString::new(share.source.as_os_str().as_bytes())?;
                check(
                    "host mount",
                    mount(ctx, tag.as_ptr(), path.as_ptr(), 0, share.read_only),
                )?;
            }
        }
        let disable =
            library.get::<unsafe extern "C" fn(u32) -> i32>(b"krun_disable_implicit_vsock\0")?;
        check("disable implicit networking", disable(ctx))?;
        let vsock = library.get::<unsafe extern "C" fn(u32, u32) -> i32>(b"krun_add_vsock\0")?;
        check("vsock without TSI", vsock(ctx, 0))?;
        let mapping = library.get::<unsafe extern "C" fn(u32, u32, *const c_char, bool) -> i32>(
            b"krun_add_vsock_port2\0",
        )?;
        check(
            "agent mapping",
            mapping(ctx, AGENT_PORT, socket.as_ptr(), true),
        )?;
        if let Some(path) = &config.egress_socket {
            let path = CString::new(path.as_os_str().as_bytes())?;
            check(
                "egress mapping",
                mapping(ctx, EGRESS_PORT, path.as_ptr(), false),
            )?;
        }
        let output = library
            .get::<unsafe extern "C" fn(u32, *const c_char) -> i32>(b"krun_set_console_output\0")?;
        check("console", output(ctx, console.as_ptr()))?;
        let workdir = library
            .get::<unsafe extern "C" fn(u32, *const c_char) -> i32>(b"krun_set_workdir\0")?;
        check("workdir", workdir(ctx, c"/".as_ptr()))?;
        let exec = library.get::<unsafe extern "C" fn(
            u32,
            *const c_char,
            *const *const c_char,
            *const *const c_char,
        ) -> i32>(b"krun_set_exec\0")?;
        let argv = [std::ptr::null()];
        let mut env = vec![c"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".as_ptr()];
        if config.egress_socket.is_some() {
            env.push(c"KUMOBA_EGRESS=1".as_ptr());
        }
        env.push(std::ptr::null());
        check(
            "exec",
            exec(
                ctx,
                c"/usr/local/bin/kumoba-guest".as_ptr(),
                argv.as_ptr(),
                env.as_ptr(),
            ),
        )?;
        let start = library.get::<unsafe extern "C" fn(u32) -> i32>(b"krun_start_enter\0")?;
        std::thread::spawn(|| {
            let mut byte = [0u8; 1];
            loop {
                match std::io::stdin().read(&mut byte) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => {
                        eprintln!("owner pipe ended: {result:?}");
                        break;
                    }
                }
            }
            std::process::exit(125);
        });
        check("start", start(ctx))?;
    }
    Err("libkrun returned without running a guest".into())
}
