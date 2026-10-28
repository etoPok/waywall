use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::wayland::ipc::{
    Compositor, CompositorEvent, CompositorEventKind, CompositorEventReader, CompositorHandler,
    wait_for_shutdown,
};
use anyhow::Context;

pub struct Hyprland {
    event_socket: PathBuf,
    request_socket: PathBuf,
    handlers: std::collections::HashMap<CompositorEventKind, Vec<CompositorHandler>>,
    reader: Option<CompositorEventReader>,
}

impl Hyprland {
    pub fn connect() -> anyhow::Result<Self> {
        let (request_socket, event_socket) = Self::sockets_from_env()?;

        let this = Self {
            request_socket,
            event_socket,
            handlers: Default::default(),
            reader: None,
        };

        this.active_workspace()
            .context("Hyprland is reachable but did not answer a request")?;
        let probe = UnixStream::connect(&this.event_socket).with_context(|| {
            format!(
                "Cannot connect to Hyprland event socket {:?}",
                this.event_socket
            )
        })?;
        drop(probe);

        Ok(this)
    }

    fn sockets_from_env() -> anyhow::Result<(PathBuf, PathBuf)> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE not set")?;
        let xdg = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
        let dir = PathBuf::from(xdg).join("hypr").join(sig);

        let request_socket = dir.join(".socket.sock");
        let event_socket = dir.join(".socket2.sock");
        if !request_socket.exists() || !event_socket.exists() {
            anyhow::bail!("Hyprland sockets not found in {:?}", dir);
        }

        Ok((request_socket, event_socket))
    }

    fn reader_loop(
        path: PathBuf,
        tx: calloop::channel::Sender<CompositorEvent>,
        shutdown: Arc<AtomicBool>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    ) {
        while !shutdown.load(Ordering::Acquire) {
            match UnixStream::connect(&path) {
                Ok(s) => {
                    let clone = match s.try_clone() {
                        Ok(clone) => clone,
                        Err(_) => {
                            wait_for_shutdown(&shutdown);
                            continue;
                        }
                    };
                    *stream.lock().unwrap() = Some(clone);

                    if shutdown.load(Ordering::Acquire) {
                        *stream.lock().unwrap() = None;
                        break;
                    }

                    let reader = BufReader::new(s);
                    for line in reader.lines() {
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }

                        let line = match line {
                            Ok(l) => l,
                            Err(_) => break,
                        };
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(event) = Self::parse_event(trimmed)
                            && tx.send(event).is_err()
                        {
                            *stream.lock().unwrap() = None;
                            return; // main loop dead
                        }
                    }

                    *stream.lock().unwrap() = None;
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    wait_for_shutdown(&shutdown);
                }
                Err(_) => {
                    wait_for_shutdown(&shutdown);
                }
            }
        }
    }

    pub fn parse_event(line: &str) -> Option<CompositorEvent> {
        let (name, data) = line.split_once(">>")?;
        match name {
            "workspace" => {
                let ws: i64 = data.parse().ok()?;
                Some(CompositorEvent::WorkspaceChanged(ws))
            }
            "activewindow" => Some(CompositorEvent::ActiveWindowChanged(data.to_string())),
            _ => None,
        }
    }
}

impl Compositor for Hyprland {
    fn subscribe(&mut self, event_type: CompositorEventKind, handler: CompositorHandler) {
        self.handlers.entry(event_type).or_default().push(handler);
    }

    fn spawn_event_reader(
        &mut self,
        tx: calloop::channel::Sender<CompositorEvent>,
    ) -> anyhow::Result<()> {
        if self.reader.is_some() {
            anyhow::bail!("Hyprland event reader is already running");
        }

        let path = self.event_socket.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));

        let shutdown_for_thread = Arc::clone(&shutdown);
        let stream_for_thread = Arc::clone(&stream);
        let thread = std::thread::Builder::new()
            .name("hyprland-events".into())
            .spawn(move || {
                Self::reader_loop(path, tx, shutdown_for_thread, stream_for_thread);
            })?;

        self.reader = Some(CompositorEventReader {
            shutdown,
            stream,
            thread: Some(thread),
        });

        Ok(())
    }

    fn dispatch(&self, event: &CompositorEvent, app: &mut crate::app::state::App) {
        let event_type = event.event_type();
        if let Some(handlers) = self.handlers.get(&event_type) {
            for handler in handlers {
                handler(app, event);
            }
        }
    }

    fn active_workspace(&self) -> anyhow::Result<i64> {
        use std::io::{Read, Write};
        let mut stream = UnixStream::connect(&self.request_socket)?;
        stream.write_all(b"j/activeworkspace")?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut buf = String::new();
        stream.read_to_string(&mut buf)?;

        let v: serde_json::Value = serde_json::from_str(&buf)?;
        v["id"].as_i64().context("missing workspace id")
    }
}
