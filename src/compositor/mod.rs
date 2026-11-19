use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use crate::compositor::{hyprland::Hyprland, niri::Niri};

pub mod hyprland;
pub mod niri;
pub mod occlusion;

mod request;

pub type OutputId = String;
pub type ToplevelId = String;
pub type WorkspaceId = i64;

pub struct CompositorEventReader {
    dispatchers: Vec<ReadEventHandler>,
    pub shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub stream: std::sync::Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
    pub thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for CompositorEventReader {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositorEventReader {
    pub fn new() -> Self {
        Self {
            dispatchers: Vec::default(),
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stream: Arc::new(Mutex::new(None)),
            thread: None,
        }
    }

    pub fn spawn<C: Compositor + Clone + Send + 'static>(
        &mut self,
        tx: calloop::channel::Sender<ReadEvent>,
        compositor: C,
    ) -> anyhow::Result<()> {
        if self.thread.is_some() {
            anyhow::bail!("Compositor event reader has already been spawned")
        }

        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));

        let shutdown_for_thread = Arc::clone(&shutdown);
        let stream_for_thread = Arc::clone(&stream);
        let thread = std::thread::Builder::new()
            .name("hyprland-events".into())
            .spawn(move || {
                compositor.reader_loop(tx, shutdown_for_thread, stream_for_thread);
            })?;

        self.shutdown = shutdown;
        self.stream = stream;
        self.thread = Some(thread);
        Ok(())
    }

    pub fn add_dispatcher(&mut self, dispatcher: ReadEventHandler) {
        self.dispatchers.push(dispatcher);
    }

    pub fn dispatch(&self, event: &ReadEvent, app: &mut crate::app::state::App) {
        for d in self.dispatchers.iter() {
            d(app, event);
        }
    }

    pub fn stop_reader(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        let socket = {
            let mut slot = self.stream.lock().unwrap();
            slot.take()
        };
        if let Some(socket) = socket {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CompositorEventReader {
    fn drop(&mut self) {
        self.stop_reader();
        tracing::info!("drop CompositorEventReader");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputStatusChange {
    Set { output: OutputId, locked: bool },
    Removed { output: OutputId },
}

#[derive(Debug, Clone)]
pub enum ReadEvent {
    ReaderKilled(()),
    CompositorEvent(CompositorEventKind),
    OutputStatusUpdated(OutputStatusChange),
    OutputStatus(Vec<(OutputId, bool)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositorEventKind {
    OutputAdded,
    OutputRemoved,
    OutputFocused,
    ActiveToplevel,
    OpenToplevel,
    CloseToplevel,
    ToplevelMoved,
    Fullscreen,
    ActiveWorkspace,
    WorkspaceMoved,
    WorkspaceCreated,
    WorkspaceDestroyed,
}

#[derive(Debug, Clone)]
pub enum CompositorEvent {
    OutputAdded(OutputId),
    OutputRemoved(OutputId),
    OutputFocused(OutputId, WorkspaceId),
    ActiveToplevel(ToplevelId),
    OpenToplevel(ToplevelId),
    CloseToplevel(ToplevelId),
    ToplevelMoved(ToplevelId, WorkspaceId),
    Fullscreen(bool),
    ActiveWorkspace(WorkspaceId),
    WorkspaceMoved(WorkspaceId, OutputId),
    WorkspaceCreated(WorkspaceId),
    WorkspaceDestroyed(WorkspaceId),
}

impl CompositorEvent {
    pub fn event_type(&self) -> CompositorEventKind {
        match self {
            Self::OutputAdded(_) => CompositorEventKind::OutputAdded,
            Self::OutputRemoved(_) => CompositorEventKind::OutputRemoved,
            Self::OutputFocused(_, _) => CompositorEventKind::OutputFocused,
            Self::ActiveToplevel(_) => CompositorEventKind::ActiveToplevel,
            Self::OpenToplevel(_) => CompositorEventKind::OpenToplevel,
            Self::CloseToplevel(_) => CompositorEventKind::CloseToplevel,
            Self::ToplevelMoved(_, _) => CompositorEventKind::ToplevelMoved,
            Self::Fullscreen(_) => CompositorEventKind::Fullscreen,
            Self::ActiveWorkspace(_) => CompositorEventKind::ActiveWorkspace,
            Self::WorkspaceMoved(_, _) => CompositorEventKind::WorkspaceMoved,
            Self::WorkspaceCreated(_) => CompositorEventKind::WorkspaceCreated,
            Self::WorkspaceDestroyed(_) => CompositorEventKind::WorkspaceDestroyed,
        }
    }
}

pub type ReadEventHandler = Box<dyn Fn(&mut crate::app::state::App, &ReadEvent)>;

pub trait Compositor {
    fn request_socket_path(&self) -> &std::path::Path;
    fn event_socket_path(&self) -> &std::path::Path;

    fn reader_loop(
        &self,
        tx: calloop::channel::Sender<ReadEvent>,
        shutdown: Arc<AtomicBool>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    );

    fn active_workspace(&self) -> anyhow::Result<WorkspaceId>;

    fn active_output(&self) -> anyhow::Result<String> {
        anyhow::bail!("active_output is not supported by this compositor")
    }

    fn active_toplevel(&self) -> anyhow::Result<Option<ToplevelId>> {
        anyhow::bail!("active_window is not supported by this compositor")
    }

    fn outputs(&self) -> anyhow::Result<Vec<(i64, String, WorkspaceId, bool)>> {
        anyhow::bail!("monitors is not supported by this compositor")
    }

    fn workspaces(&self) -> anyhow::Result<Vec<(WorkspaceId, OutputId)>> {
        anyhow::bail!("workspaces is not supported by this compositor")
    }
}

#[derive(Clone)]
pub enum AnyCompositor {
    Hyprland(Hyprland),
    Niri(Niri),
}

impl Compositor for AnyCompositor {
    fn request_socket_path(&self) -> &std::path::Path {
        match self {
            AnyCompositor::Hyprland(h) => h.request_socket_path(),
            AnyCompositor::Niri(n) => n.request_socket_path(),
        }
    }

    fn event_socket_path(&self) -> &std::path::Path {
        match self {
            AnyCompositor::Hyprland(h) => h.event_socket_path(),
            AnyCompositor::Niri(n) => n.event_socket_path(),
        }
    }

    fn reader_loop(
        &self,
        tx: calloop::channel::Sender<ReadEvent>,
        shutdown: Arc<AtomicBool>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    ) {
        match self {
            AnyCompositor::Hyprland(h) => h.reader_loop(tx, shutdown, stream),
            AnyCompositor::Niri(n) => n.reader_loop(tx, shutdown, stream),
        }
    }

    fn active_workspace(&self) -> anyhow::Result<WorkspaceId> {
        match self {
            AnyCompositor::Hyprland(h) => h.active_workspace(),
            AnyCompositor::Niri(n) => n.active_workspace(),
        }
    }

    fn active_output(&self) -> anyhow::Result<String> {
        match self {
            AnyCompositor::Hyprland(h) => h.active_output(),
            AnyCompositor::Niri(n) => n.active_output(),
        }
    }

    fn active_toplevel(&self) -> anyhow::Result<Option<ToplevelId>> {
        match self {
            AnyCompositor::Hyprland(h) => h.active_toplevel(),
            AnyCompositor::Niri(n) => n.active_toplevel(),
        }
    }

    fn outputs(&self) -> anyhow::Result<Vec<(i64, String, WorkspaceId, bool)>> {
        match self {
            AnyCompositor::Hyprland(h) => h.outputs(),
            AnyCompositor::Niri(n) => n.outputs(),
        }
    }

    fn workspaces(&self) -> anyhow::Result<Vec<(WorkspaceId, OutputId)>> {
        match self {
            AnyCompositor::Hyprland(h) => h.workspaces(),
            AnyCompositor::Niri(n) => n.workspaces(),
        }
    }
}

pub type CompositorChannel = (
    calloop::channel::Sender<ReadEvent>,
    calloop::channel::Channel<ReadEvent>,
);
