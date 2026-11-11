//! The named pipe, its ACL, and a LocalSystem service behind it.
//!
//! ```text
//! pipesvc whoami                          this process: user SID and session
//! pipesvc serve <pipe> <sddl|-> [--not-first]   a server in the foreground
//! pipesvc squat <pipe>                    a server with the default ACL, 60 seconds
//! pipesvc client <pipe> <message> [--as <user> <password>]
//! pipesvc probe <pipe>                    open the pipe, print who serves it
//! pipesvc install <pipe> <sid>            register this exe as a service and start it
//! pipesvc uninstall
//! pipesvc service <pipe> <sid>            the entry point the SCM starts
//! ```
//!
//! Messages are text. The first word is the op: `whoami`, `hosts-rename
//! <name>`, `hosts-inplace <name>`, `hosts-clear`, `trust` (then the PEM on
//! the next lines), `untrust <id>`, `store <id>` (is it in the store?), `stop`. The reply starts with `ok` or
//! `err`. The service allows SYSTEM and the SID given at install.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    imp::main()
}

#[cfg(windows)]
mod imp {
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::process::{Command, ExitCode, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use s11_windows::{
        PipeServer, current_session, impersonate_user, pipe_server_of, process_sid, read_message,
        system_directory, write_message,
    };
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    const SERVICE: &str = "MarsaSpikeHelper";
    const BEGIN: &str = "# Marsa spike start";
    const END: &str = "# Marsa spike end";
    /// From the Win32 `CreateFile` page: the server may identify the client
    /// but not act as it.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    const ERROR_ACCESS_DENIED: i32 = 5;

    type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

    pub fn main() -> ExitCode {
        let args: Vec<String> = env::args().skip(1).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = match args.as_slice() {
            ["whoami"] => whoami(),
            ["serve", pipe, sddl, rest @ ..] => serve_foreground(pipe, sddl, rest),
            ["squat", pipe] => squat(pipe),
            ["probe", pipe] => return client(pipe, "", None),
            ["client", pipe, message] => return client(pipe, message, None),
            ["client", pipe, message, "--as", user, password] => {
                return client(pipe, message, Some((user, password)));
            }
            ["install", pipe, sid] => install(pipe, sid),
            ["uninstall"] => uninstall(),
            ["service", ..] => service_dispatcher::start(SERVICE, ffi_service_main)
                .map_err(|err| format!("dispatcher: {err:?}").into()),
            _ => Err("usage: see the top of pipesvc.rs".into()),
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                println!("error: {err}");
                ExitCode::FAILURE
            }
        }
    }

    fn whoami() -> AnyResult<()> {
        println!("sid={} session={}", process_sid()?, current_session()?);
        Ok(())
    }

    fn sddl_of(text: &str) -> Option<&str> {
        (text != "-").then_some(text)
    }

    fn serve_foreground(pipe: &str, sddl: &str, rest: &[&str]) -> AnyResult<()> {
        let first = !rest.contains(&"--not-first");
        let server = PipeServer::create(pipe, sddl_of(sddl), first)
            .map_err(|err| format!("create pipe: {err} (os {:?})", err.raw_os_error()))?;
        println!("serving {pipe}");
        serve(&server);
        Ok(())
    }

    fn squat(pipe: &str) -> AnyResult<()> {
        let _server = PipeServer::create(pipe, None, true)
            .map_err(|err| format!("create pipe: {err} (os {:?})", err.raw_os_error()))?;
        println!("squatting {pipe}");
        thread::sleep(Duration::from_secs(60));
        Ok(())
    }

    /// Serves one client at a time until a `stop` message.
    fn serve(server: &PipeServer) {
        loop {
            if server.accept().is_err() {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let stop = match server.read_message() {
                Ok(message) => {
                    let message = String::from_utf8_lossy(&message).into_owned();
                    // After the first read, so the client can be impersonated.
                    let reply = handle(server, &message);
                    let _ = server.write_message(reply.as_bytes());
                    message == "stop"
                }
                Err(_) => false,
            };
            server.disconnect();
            if stop {
                return;
            }
        }
    }

    fn handle(server: &PipeServer, message: &str) -> String {
        let (op, rest) = message.split_once([' ', '\n']).unwrap_or((message, ""));
        let result = match op {
            "whoami" => Ok(format!(
                "sid={:?} session={:?} sysdir={:?} client={:?}",
                process_sid(),
                current_session(),
                system_directory(),
                server.client(),
            )),
            "hosts-rename" => write_hosts(Some(rest), true),
            "hosts-inplace" => write_hosts(Some(rest), false),
            "hosts-clear" => write_hosts(None, true),
            "trust" => trust(rest),
            "untrust" => untrust(rest),
            "store" => certutil(&[OsStr::new("-store"), OsStr::new("Root"), OsStr::new(rest)]),
            "stop" => Ok("stopping".to_owned()),
            _ => Err(io::Error::other("unknown op")),
        };
        match result {
            Ok(text) => format!("ok {text}"),
            Err(err) => format!("err {err} (os {:?})", err.raw_os_error()),
        }
    }

    fn hosts_path() -> io::Result<PathBuf> {
        Ok(PathBuf::from(system_directory()?)
            .join("drivers")
            .join("etc")
            .join("hosts"))
    }

    /// The hosts text without the spike's block, then the new block if any.
    fn hosts_text(current: &str, name: Option<&str>) -> String {
        let mut out = String::new();
        let mut inside = false;
        for line in current.lines() {
            match line.trim() {
                BEGIN => inside = true,
                END => inside = false,
                _ if !inside => {
                    out.push_str(line);
                    out.push_str("\r\n");
                }
                _ => {}
            }
        }
        if let Some(name) = name {
            out.push_str(&format!("{BEGIN}\r\n127.0.0.1 {name}\r\n{END}\r\n"));
        }
        out
    }

    fn write_hosts(name: Option<&str>, rename: bool) -> io::Result<String> {
        let path = hosts_path()?;
        let meta = fs::metadata(&path)?;
        let current = fs::read_to_string(&path)?;
        let text = hosts_text(&current, name);
        if rename {
            let tmp = path.with_file_name(".hosts.marsa-spike-tmp");
            fs::write(&tmp, &text)?;
            fs::rename(&tmp, &path)?;
        } else {
            fs::write(&path, &text)?;
        }
        Ok(format!(
            "wrote {} bytes to {} (was {} bytes, readonly={})",
            text.len(),
            path.display(),
            current.len(),
            meta.permissions().readonly(),
        ))
    }

    fn certutil(args: &[&OsStr]) -> io::Result<String> {
        let program = PathBuf::from(system_directory()?).join("certutil.exe");
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok(format!(
            "exit={:?} stdout={:?} stderr={:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ))
    }

    fn trust(pem: &str) -> io::Result<String> {
        let dir = env::var_os("ProgramData").ok_or_else(|| io::Error::other("no ProgramData"))?;
        let dir = PathBuf::from(dir).join("MarsaSpike");
        fs::create_dir_all(&dir)?;
        let file = dir.join("ca-root.pem");
        fs::write(&file, pem)?;
        certutil(&[
            OsStr::new("-addstore"),
            OsStr::new("Root"),
            file.as_os_str(),
        ])
    }

    fn untrust(id: &str) -> io::Result<String> {
        certutil(&[OsStr::new("-delstore"), OsStr::new("Root"), OsStr::new(id)])
    }

    /// Opens the pipe like the daemon would, and prints what it sees. Exit
    /// code: 0 the server answered, 5 access denied, 1 anything else.
    fn client(pipe: &str, message: &str, user: Option<(&str, &str)>) -> ExitCode {
        if let Some((user, password)) = user {
            if let Err(err) = impersonate_user(user, password) {
                println!("logon=err {err}");
                return ExitCode::FAILURE;
            }
            println!("logon=ok user={user}");
        }
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(format!(r"\\.\pipe\{pipe}"));
        let mut file = match opened {
            Ok(file) => file,
            Err(err) => {
                println!("open=err os={:?} {err}", err.raw_os_error());
                return if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
                    ExitCode::from(5)
                } else {
                    ExitCode::FAILURE
                };
            }
        };
        let (session, pid) = pipe_server_of(&file);
        println!("open=ok server_session={session:?} server_pid={pid:?}");
        if message.is_empty() {
            // `probe`: only who is behind the pipe.
            return ExitCode::SUCCESS;
        }
        let reply = write_message(&mut file, message.replace("\\n", "\n").as_bytes())
            .and_then(|()| read_message(&mut file));
        match reply {
            Ok(reply) => {
                println!("reply={}", String::from_utf8_lossy(&reply));
                ExitCode::SUCCESS
            }
            Err(err) => {
                println!("reply=err {err}");
                ExitCode::FAILURE
            }
        }
    }

    fn install(pipe: &str, sid: &str) -> AnyResult<()> {
        let access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
        let manager = ServiceManager::local_computer(None::<&str>, access)?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE),
            display_name: OsString::from("Marsa spike helper"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: env::current_exe()?,
            launch_arguments: vec!["service".into(), pipe.into(), sid.into()],
            dependencies: vec![],
            // `None`: the LocalSystem account.
            account_name: None,
            account_password: None,
        };
        let service =
            manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
        service.set_description("S-11 spike: hosts file and CA trust behind a named pipe")?;
        service.start::<&OsStr>(&[])?;
        println!("installed and started {SERVICE}");
        Ok(())
    }

    fn uninstall() -> AnyResult<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
        let service = manager.open_service(SERVICE, access)?;
        // Already stopped is fine.
        let stopped = service.stop();
        service.delete()?;
        println!("deleted {SERVICE} (stop: {stopped:?})");
        Ok(())
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_start_arguments: Vec<OsString>) {
        // The launch arguments from the service's command line.
        let args: Vec<String> = env::args().skip(2).collect();
        let (Some(pipe), Some(sid)) = (args.first(), args.get(1)) else {
            return;
        };
        let (stop_sender, stop_receiver) = mpsc::channel();
        let handler = move |control| match control {
            ServiceControl::Stop => {
                let _ = stop_sender.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(SERVICE, handler) else {
            return;
        };
        let report = move |state, accepted| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accepted,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            });
        };
        // SYSTEM, and read and write for the installing user; nobody else.
        // `P`: no inherited ACEs.
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{sid})");
        let server = match PipeServer::create(pipe, Some(&sddl), true) {
            Ok(server) => server,
            Err(err) => {
                // Shows up in `sc query` as the service's exit code.
                let _ = status.set_service_status(ServiceStatus {
                    service_type: ServiceType::OWN_PROCESS,
                    current_state: ServiceState::Stopped,
                    controls_accepted: ServiceControlAccept::empty(),
                    exit_code: ServiceExitCode::Win32(err.raw_os_error().unwrap_or(1) as u32),
                    checkpoint: 0,
                    wait_hint: Duration::default(),
                    process_id: None,
                });
                return;
            }
        };
        report(ServiceState::Running, ServiceControlAccept::STOP);
        // The pipe loop blocks in `accept`, so a stop request ends the
        // process from this thread. Good enough for a spike.
        thread::spawn(move || {
            let _ = stop_receiver.recv();
            report(ServiceState::Stopped, ServiceControlAccept::empty());
            std::process::exit(0);
        });
        serve(&server);
        report(ServiceState::Stopped, ServiceControlAccept::empty());
    }
}
