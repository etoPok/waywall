use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::wayland::ipc::{
    Compositor, CompositorEvent, CompositorEventKind, CompositorEventReader, CompositorHandler,
    wait_for_shutdown,
};
use anyhow::Context;

pub struct Niri {
    socket_dir: PathBuf,
    handlers: std::collections::HashMap<CompositorEventKind, Vec<CompositorHandler>>,
    reader: Option<CompositorEventReader>,
}

impl Niri {
    pub fn connect() -> anyhow::Result<Self> {
        let socket_dir = Self::socket_from_env()?;
        let this = Self {
            socket_dir,
            handlers: Default::default(),
            reader: None,
        };

        this.active_workspace()
            .context("Niri is reachable but did not answer a request")?;
        let probe = UnixStream::connect(&this.socket_dir).with_context(|| {
            format!("Cannot connect to Niri event socket {:?}", this.socket_dir)
        })?;
        drop(probe);

        Ok(this)
    }

    fn socket_from_env() -> anyhow::Result<PathBuf> {
        let niri = std::env::var("NIRI_SOCKET")?;
        let dir = PathBuf::from(niri);

        if !dir.exists() {
            anyhow::bail!("Niri socket not found in {:?}", dir);
        }

        Ok(dir)
    }

    fn reader_loop(
        path: PathBuf,
        tx: calloop::channel::Sender<CompositorEvent>,
        shutdown: Arc<AtomicBool>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    ) {
        fn connect_and_read(
            path: &PathBuf,
            tx: &calloop::channel::Sender<CompositorEvent>,
            shutdown: &Arc<AtomicBool>,
            stream: &Arc<Mutex<Option<UnixStream>>>,
        ) -> Result<(), anyhow::Error> {
            let s = UnixStream::connect(path)?;
            let clone = s.try_clone()?;
            *stream.lock().unwrap() = Some(clone);

            s.set_read_timeout(Some(Duration::from_millis(500)))?;

            let mut reader = BufReader::new(s);
            let mut request_buf = serde_json::to_string(&niri_ipc::Request::EventStream)?;
            request_buf.push('\n');
            reader.get_mut().write_all(request_buf.as_bytes())?;

            let mut reply_buf = String::new();
            reader.read_line(&mut reply_buf)?;
            let reply: niri_ipc::Reply = serde_json::from_str(&reply_buf)?;

            match reply {
                Ok(niri_ipc::Response::Handled) => {
                    reader.get_mut().shutdown(std::net::Shutdown::Write)?;

                    let mut event_buf = String::new();
                    loop {
                        if shutdown.load(Ordering::Acquire) {
                            return Ok(());
                        }

                        event_buf.clear();
                        match reader.read_line(&mut event_buf) {
                            Ok(0) => {
                                // EOF, socket close
                                *stream.lock().unwrap() = None;
                                return Ok(());
                            }
                            Ok(_) => match Niri::parse_event(&event_buf) {
                                Some(event) => {
                                    if tx.send(event).is_err() {
                                        *stream.lock().unwrap() = None;
                                        return Ok(());
                                    }
                                }
                                None => {
                                    continue;
                                }
                            },
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // Timeout, continue check shutdown
                                continue;
                            }
                            Err(e) => {
                                // Read error
                                *stream.lock().unwrap() = None;
                                return Err(e.into());
                            }
                        }
                    }
                }
                Ok(response) => {
                    anyhow::bail!("Unexpected response: {:?}", response);
                }
                Err(error_msg) => {
                    anyhow::bail!("Request failed: {}", error_msg);
                }
            }
        }

        while !shutdown.load(Ordering::Acquire) {
            match connect_and_read(&path, &tx, &shutdown, &stream) {
                Ok(()) => {
                    if !shutdown.load(Ordering::Acquire) {
                        wait_for_shutdown(&shutdown);
                        tracing::warn!("Niri connection closed, reconnecting...");
                    }
                }
                Err(e) => {
                    tracing::error!("Error in Niri event reading loop: {}", e);
                    if !shutdown.load(Ordering::Acquire) {
                        wait_for_shutdown(&shutdown);
                    }
                }
            }
        }
    }

    pub fn parse_event(line: &str) -> Option<CompositorEvent> {
        let event = serde_json::from_str::<niri_ipc::Event>(line)
            .context("Failed to parse Niri event from raw line")
            .ok()?;

        match event {
            niri_ipc::Event::WorkspaceActivated { id, focused: _ } => {
                Some(CompositorEvent::WorkspaceChanged(id as i64))
            }
            _ => None,
        }
    }
}

impl Compositor for Niri {
    fn subscribe(&mut self, event_type: CompositorEventKind, handler: CompositorHandler) {
        self.handlers.entry(event_type).or_default().push(handler);
    }

    fn spawn_event_reader(
        &mut self,
        tx: calloop::channel::Sender<CompositorEvent>,
    ) -> anyhow::Result<()> {
        if self.reader.is_some() {
            anyhow::bail!("Niri event reader is already running");
        }

        let path = self.socket_dir.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));

        let shutdown_for_thread = Arc::clone(&shutdown);
        let stream_for_thread = Arc::clone(&stream);
        let thread = std::thread::Builder::new()
            .name("niri-events".into())
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
        use std::io::{BufRead, BufReader, Write};

        let mut stream = UnixStream::connect(&self.socket_dir)?;
        let mut writer = BufReader::new(&mut stream);

        let mut request_buf = serde_json::to_string(&niri_ipc::Request::Workspaces)?;
        request_buf.push('\n');
        writer.get_mut().write_all(request_buf.as_bytes())?;

        let mut reply_buf = String::new();
        writer.read_line(&mut reply_buf)?;

        let reply: niri_ipc::Reply = serde_json::from_str(&reply_buf)?;

        match reply {
            Ok(niri_ipc::Response::Workspaces(workspaces)) => workspaces
                .iter()
                .find(|w| w.is_focused)
                .map(|w| w.id as i64)
                .context("No active workspace found"),
            Ok(other) => {
                anyhow::bail!("Unexpected response: {:?}", other);
            }
            Err(error_msg) => {
                anyhow::bail!("Request failed: {}", error_msg);
            }
        }
    }
}
