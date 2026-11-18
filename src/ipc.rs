//! Tiny JSON-lines control protocol over `$XDG_RUNTIME_DIR/veil.sock`.

use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::config::Mode;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Pause,
    Resume,
    Toggle,
    Set {
        path: Option<String>,
        mode: Option<Mode>,
        output: Option<String>,
    },
    Reload,
    Quit,
}

pub struct Message {
    pub request: Request,
    pub reply: Sender<String>,
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("veil.sock")
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    pub fn bind() -> Result<(Self, Receiver<Message>, Sender<Message>), String> {
        let path = socket_path();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (tx, rx) = mpsc::channel();
        Ok((
            Server { listener, path },
            rx,
            tx,
        ))
    }

    pub fn fd(&self) -> i32 {
        self.listener.as_raw_fd()
    }

    pub fn accept(&self, tx: &Sender<Message>, wake_fd: i32) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => spawn_client(stream, tx.clone(), wake_fd),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    log::warn!("control socket: {error}");
                    break;
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn spawn_client(stream: UnixStream, tx: Sender<Message>, wake_fd: i32) {
    std::thread::spawn(move || {
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }

        let request = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => request,
            Err(error) => {
                let payload = serde_json::json!({ "ok": false, "error": error.to_string() });
                let _ = writeln!(writer, "{payload}");
                return;
            }
        };

        let (reply_tx, reply_rx) = mpsc::channel();
        if tx.send(Message { request, reply: reply_tx }).is_err() {
            return;
        }
        crate::wake(wake_fd);

        let response = reply_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| r#"{"ok":false,"error":"daemon did not answer"}"#.to_string());
        let _ = writeln!(writer, "{response}");
    });
}
