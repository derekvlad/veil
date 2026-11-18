//! Tiny JSON-lines control protocol over `$XDG_RUNTIME_DIR/veil.sock`.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use crate::config::Mode;

#[derive(Debug, Deserialize, Serialize)]
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
        /// Drop every per-monitor override before applying the wallpaper.
        #[serde(default)]
        clear_outputs: bool,
    },
    /// Advance the playlist on one monitor (or on all of them).
    Next {
        output: Option<String>,
    },
    /// Switch to a random wallpaper from the playlist.
    Random {
        output: Option<String>,
    },
    /// Drop a per-monitor override; `None` clears all of them.
    Clear {
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

/// Send one request to a running daemon and decode its JSON answer.
///
/// This is the client half of the protocol, shared by `veilctl` and the GUI.
pub fn call(request: &Request) -> Result<serde_json::Value, String> {
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .map_err(|error| format!("{}: {error} (is veild running?)", path.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(12)))
        .map_err(|error| format!("{}: {error}", path.display()))?;

    let payload = serde_json::to_string(request).map_err(|error| error.to_string())?;
    let mut writer = stream
        .try_clone()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    writeln!(writer, "{payload}").map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.trim().is_empty() {
        return Err("daemon closed the connection without answering".into());
    }
    serde_json::from_str(line.trim()).map_err(|error| format!("bad answer: {error}"))
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
        Ok((Server { listener, path }, rx, tx))
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
        if tx
            .send(Message {
                request,
                reply: reply_tx,
            })
            .is_err()
        {
            return;
        }
        crate::wake(wake_fd);

        let response = reply_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| r#"{"ok":false,"error":"daemon did not answer"}"#.to_string());
        let _ = writeln!(writer, "{response}");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(request: Request) {
        let text = serde_json::to_string(&request).unwrap();
        let parsed: Request = serde_json::from_str(&text).unwrap();
        assert_eq!(
            std::mem::discriminant(&request),
            std::mem::discriminant(&parsed)
        );
    }

    #[test]
    fn control_messages_serialise() {
        round_trip(Request::Status);
        round_trip(Request::Pause);
        round_trip(Request::Resume);
        round_trip(Request::Toggle);
        round_trip(Request::Reload);
        round_trip(Request::Quit);
        round_trip(Request::Next {
            output: Some("DP-1".into()),
        });
        round_trip(Request::Next { output: None });
        round_trip(Request::Random { output: None });
        round_trip(Request::Clear { output: None });
        round_trip(Request::Set {
            path: Some("/tmp/a.mp4".into()),
            mode: Some(Mode::Fit),
            output: Some("HDMI-A-1".into()),
            clear_outputs: true,
        });
    }

    #[test]
    fn optional_fields_may_be_omitted() {
        let parsed: Request = serde_json::from_str(r#"{"cmd":"next"}"#).unwrap();
        assert!(matches!(parsed, Request::Next { output: None }));
        let parsed: Request = serde_json::from_str(r#"{"cmd":"clear"}"#).unwrap();
        assert!(matches!(parsed, Request::Clear { output: None }));
    }
}
