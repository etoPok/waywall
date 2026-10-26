pub mod hyprland;

pub struct CompositorEventReader {
    pub shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub stream: std::sync::Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
    pub thread: std::thread::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositorEventKind {
    WorkspaceChanged,
    ActiveWindowChanged,
}

#[derive(Debug, Clone)]
pub enum CompositorEvent {
    WorkspaceChanged(i64),
    ActiveWindowChanged(String),
}

impl CompositorEvent {
    pub fn event_type(&self) -> CompositorEventKind {
        match self {
            Self::WorkspaceChanged(_) => CompositorEventKind::WorkspaceChanged,
            Self::ActiveWindowChanged(_) => CompositorEventKind::ActiveWindowChanged,
        }
    }
}

pub type CompositorHandler = Box<dyn Fn(&mut crate::app::state::App, &CompositorEvent)>;

pub trait Compositor {
    /// Register a handler that is invoked when dispatching the event
    fn subscribe(&mut self, event_type: CompositorEventKind, handler: CompositorHandler);

    /// Spawns thread for reading the event stream
    fn spawn_event_reader(
        &mut self,
        tx: calloop::channel::Sender<CompositorEvent>,
    ) -> anyhow::Result<()>;

    /// Dispatches the specified event by invoking its registering handlers.
    fn dispatch(&self, event: &CompositorEvent, app: &mut crate::app::state::App);

    /// Queries the compositor backend for the active workspace identifier
    fn active_workspace(&self) -> anyhow::Result<i64>;
}

pub type CompositorChannel = (
    calloop::channel::Sender<CompositorEvent>,
    calloop::channel::Channel<CompositorEvent>,
);
