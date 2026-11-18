//! `veild` — the wallpaper daemon.

use std::io::Write;
use std::os::raw::{c_int, c_void};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use veil::config::Config;
use veil::egl::Egl;
use veil::ipc::{self, Request};
use veil::wayland::App;
use wayland_client::Connection;

static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static QUIT: AtomicBool = AtomicBool::new(false);
static RELOAD: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(signal: c_int) {
    match signal {
        libc::SIGHUP => RELOAD.store(true, Ordering::Relaxed),
        _ => QUIT.store(true, Ordering::Relaxed),
    }
    veil::wake(WAKE_FD.load(Ordering::Relaxed));
}

struct Logger {
    level: log::LevelFilter,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{:<5}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn main() {
    let mut config_path: Option<PathBuf> = None;
    let mut verbose = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-v" | "--verbose" => verbose = true,
            "--config" => config_path = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                help();
                return;
            }
            "-V" | "--version" => {
                println!("veild {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("veild: unknown argument `{other}` (try --help)");
                std::process::exit(2);
            }
        }
    }

    let logger = Logger {
        level: if verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        },
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(if verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        });
    }

    // One daemon per session: niri's spawn-at-startup and the systemd unit may
    // both fire, and two daemons would fight over the socket and the layer.
    let _instance = match lock_instance() {
        Ok(lock) => lock,
        Err(error) => {
            log::info!("{error}");
            return;
        }
    };

    let loaded = match &config_path {
        Some(path) => Config::load_from(path),
        None => Config::load(),
    };
    let (config, path) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veild: {error}");
            std::process::exit(1);
        }
    };

    if let Err(error) = run(config, path) {
        eprintln!("veild: {error}");
        std::process::exit(1);
    }
}

fn help() {
    println!(
        "veild {} — video wallpapers for Wayland\n\
         \n\
         Usage: veild [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 -v, --verbose        log decoding details\n\
         \x20     --config <file>  use an alternative config file\n\
         \x20 -h, --help           show this help\n\
         \x20 -V, --version        print the version",
        env!("CARGO_PKG_VERSION")
    );
}

fn lock_instance() -> Result<std::fs::File, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let path = dir.join("veil.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == libc::EWOULDBLOCK) {
            return Err(format!(
                "another veild is already running ({})",
                path.display()
            ));
        }
        return Err(format!("{}: {error}", path.display()));
    }

    let mut writer = &file;
    let _ = writer.set_len(0);
    let _ = writeln!(writer, "{}", std::process::id());
    Ok(file)
}

fn run(config: Config, config_path: PathBuf) -> Result<(), String> {
    let connection = Connection::connect_to_env()
        .map_err(|error| format!("cannot connect to Wayland: {error}"))?;
    let wl_display = connection.backend().display_ptr() as *mut c_void;

    let wake_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if wake_fd < 0 {
        return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
    }
    WAKE_FD.store(wake_fd, Ordering::Relaxed);

    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as extern "C" fn(c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as extern "C" fn(c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGHUP,
            handle_signal as extern "C" fn(c_int) as libc::sighandler_t,
        );
    }

    let egl = Egl::new(wl_display)?;

    let mut queue = connection.new_event_queue::<App>();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let mut app = App::new(config, config_path, egl, wl_display, qh.clone(), wake_fd);
    app.registry = Some(registry);

    // First roundtrip collects globals, the second one delivers layer-shell
    // configures so the first frame is on screen as early as possible.
    queue
        .roundtrip(&mut app)
        .map_err(|error| format!("Wayland roundtrip failed: {error}"))?;
    app.process_pending();
    queue
        .roundtrip(&mut app)
        .map_err(|error| format!("Wayland roundtrip failed: {error}"))?;
    app.process_pending();
    app.tick();

    if !app.cfg.has_wallpaper() {
        log::warn!("no wallpaper configured; use `veilctl set <file>`");
    }

    let (server, ipc_rx, ipc_tx) = ipc::Server::bind()?;
    log::info!("listening on {}", ipc::socket_path().display());

    let connection_fd = connection.backend().poll_fd().as_raw_fd();
    let ipc_fd = server.fd();

    loop {
        queue
            .dispatch_pending(&mut app)
            .map_err(|error| format!("Wayland dispatch failed: {error}"))?;
        let _ = connection.flush();

        app.process_pending();
        app.paint_dirty();

        while let Ok(message) = ipc_rx.try_recv() {
            let response = handle_request(&mut app, message.request);
            let _ = message.reply.send(response);
        }

        if app.exit || QUIT.load(Ordering::Relaxed) {
            break;
        }
        if RELOAD.swap(false, Ordering::Relaxed) {
            app.reload();
        }

        let mut fds = [
            libc::pollfd {
                fd: connection_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: ipc_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 1000) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("poll: {error}"));
        }

        if fds[1].revents & libc::POLLIN != 0 {
            drain(wake_fd);
        }
        if fds[2].revents & libc::POLLIN != 0 {
            server.accept(&ipc_tx, wake_fd);
        }
        if fds[0].revents & libc::POLLIN != 0 {
            if let Some(guard) = connection.prepare_read() {
                let _ = guard.read();
            }
        }

        app.tick();
    }

    log::info!("shutting down");
    drop(server);
    Ok(())
}

fn drain(fd: i32) {
    let mut buffer = [0u8; 8];
    loop {
        let read = unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut c_void, 8) };
        if read <= 0 {
            break;
        }
    }
}

fn handle_request(app: &mut App, request: Request) -> String {
    use serde_json::json;
    match request {
        Request::Status => app.status().to_string(),
        Request::Pause => {
            app.manual_paused = true;
            app.refresh_pause();
            json!({ "ok": true, "paused": true }).to_string()
        }
        Request::Resume => {
            app.manual_paused = false;
            app.refresh_pause();
            json!({ "ok": true, "paused": false }).to_string()
        }
        Request::Toggle => {
            app.manual_paused = !app.manual_paused;
            app.refresh_pause();
            let paused = app.manual_paused;
            json!({ "ok": true, "paused": paused }).to_string()
        }
        Request::Set {
            path,
            mode,
            output,
            clear_outputs,
        } => {
            if clear_outputs {
                app.clear_overrides();
            }
            match (path, output) {
                (Some(path), Some(output)) => app.set_output_wallpaper(&output, path),
                (Some(path), None) => app.set_wallpaper(Some(path), mode),
                (None, _) => app.set_wallpaper(None, mode),
            }
            json!({ "ok": true }).to_string()
        }
        Request::Reload => {
            app.reload();
            json!({ "ok": true }).to_string()
        }
        Request::Quit => {
            app.exit = true;
            json!({ "ok": true, "bye": true }).to_string()
        }
    }
}
