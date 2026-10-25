use std::cell::RefCell;
use std::io::Write;
use std::mem::ManuallyDrop;
use std::rc::Rc;
use std::time::Duration;

use calloop::EventLoop;
use calloop::channel::{self, Event as ChannelEvent};
use calloop::timer::{TimeoutAction, Timer};
use waywall::app::state::App;
use waywall::decoder::Decoder;
use waywall::timing::Timing;
use waywall::wayland::connection::WlConnection;
use waywall::wayland::ipc::hyprland::Hyprland;
use waywall::wayland::ipc::{Compositor, CompositorEvent, CompositorEventKind};

struct TestState {
    app: ManuallyDrop<App>,
}

fn hyprland_env_available() -> bool {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        eprintln!("skipping: HYPRLAND_INSTANCE_SIGNATURE is not set");
        return false;
    }

    true
}

fn wayland_env_available() -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipping: WAYLAND_DISPLAY is not set");
        return false;
    }

    true
}

fn build_test_app() -> anyhow::Result<App> {
    let (conn, globals, queue) = WlConnection::connect()?;
    let (compositor, layer_shell) =
        WlConnection::bind_required_protocol_extensions(&globals, &queue)?;
    let wl_conn = WlConnection::new(conn, globals, queue.handle(), compositor, layer_shell);

    Ok(App::new(wl_conn, Decoder::new(), Timing::new()))
}

#[test]
fn parses_workspace_event() {
    assert!(matches!(
        Hyprland::parse_event("workspace>>3"),
        Some(CompositorEvent::WorkspaceChanged(3))
    ));
}

#[test]
fn parses_activewindow_event() {
    assert!(matches!(
        Hyprland::parse_event("activewindow>>firefox,Example"),
        Some(CompositorEvent::ActiveWindowChanged(data)) if data == "firefox,Example"
    ));
}

#[test]
fn ignores_unknown_event() {
    assert!(Hyprland::parse_event("monitoradded>>eDP-1").is_none());
}

#[test]
fn connects_to_live_hyprland() {
    if !hyprland_env_available() {
        return;
    }

    let _hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
}

#[test]
fn active_workspace_request_returns_workspace_id() {
    if !hyprland_env_available() {
        return;
    }

    let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
    let id = hyprland
        .active_workspace()
        .expect("active_workspace request failed");
    assert_ne!(id, 0, "active workspace id should not be 0");

    let id_again = hyprland
        .active_workspace()
        .expect("second active_workspace request failed");
    assert_eq!(id, id_again, "active workspace changed between requests");
}

#[test]
#[ignore = "requires user interaction"]
fn workspace_changed_event_reaches_subscribed_handler() {
    if !hyprland_env_available() || !wayland_env_available() {
        return;
    }

    let mut hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
    let app = build_test_app().expect("failed to build test App");

    let received = Rc::new(RefCell::new(Vec::new()));
    let received_for_handler = Rc::clone(&received);
    hyprland.subscribe(
        CompositorEventKind::WorkspaceChanged,
        Box::new(move |_app, event| {
            received_for_handler.borrow_mut().push(event.clone());
        }),
    );

    let (tx, rx) = channel::channel::<CompositorEvent>();
    hyprland
        .spawn_event_reader(tx)
        .expect("failed to spawn Hyprland event reader");

    let mut event_loop: EventLoop<'static, TestState> =
        EventLoop::try_new().expect("failed to create event loop");
    let loop_signal = event_loop.get_signal();

    let loop_signal_for_events = loop_signal.clone();
    event_loop
        .handle()
        .insert_source(rx, move |event, _, state| {
            if let ChannelEvent::Msg(event) = event {
                eprintln!("event received: {event:?}");
                hyprland.dispatch(&event, &mut state.app);

                if matches!(event, CompositorEvent::WorkspaceChanged(_)) {
                    loop_signal_for_events.stop();
                }
            }
        })
        .expect("failed to insert channel source");

    let loop_signal_for_timeout = loop_signal.clone();
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_secs(120)),
            move |_, _, _| {
                eprintln!("timeout: no WorkspaceChanged event received");
                loop_signal_for_timeout.stop();
                TimeoutAction::Drop
            },
        )
        .expect("failed to insert timeout timer");

    println!("Switch workspace now. Waiting 120s...");
    let _ = std::io::stdout().flush();

    let mut state = TestState {
        app: ManuallyDrop::new(app),
    };
    event_loop
        .run(None, &mut state, |_| {})
        .expect("event loop failed");

    drop(event_loop);

    let events = received.borrow();
    assert!(
        !events.is_empty(),
        "no WorkspaceChanged event received; did you switch workspaces?"
    );

    match events.last().expect("event list is empty") {
        CompositorEvent::WorkspaceChanged(id) => {
            assert_ne!(*id, 0, "workspace id should not be 0");
        }
        other => panic!("handler received unexpected event: {other:?}"),
    }
}
