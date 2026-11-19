use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::request::{Framing, request_json, wait_for_shutdown};
use crate::compositor::occlusion::OcclusionTracker;
use crate::compositor::{
    Compositor, CompositorEvent, OutputId, ReadEvent, ToplevelId, WorkspaceId,
};
use anyhow::Context;

type Client = (ToplevelId, bool, WorkspaceId, i64);

#[derive(Clone, Debug)]
pub struct Hyprland {
    event_socket_path: PathBuf,
    request_socket_path: PathBuf,
}

impl Hyprland {
    pub fn connect() -> anyhow::Result<Self> {
        let (request_socket, event_socket) = Self::sockets_from_env()?;

        let this = Self {
            request_socket_path: request_socket,
            event_socket_path: event_socket,
        };

        this.active_workspace()
            .context("Hyprland is reachable but did not answer a request")?;
        let probe = UnixStream::connect(&this.event_socket_path).with_context(|| {
            format!(
                "Cannot connect to Hyprland event socket {:?}",
                this.event_socket_path
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

    pub fn parse_event(line: &str) -> Option<CompositorEvent> {
        let (name, data) = line.split_once(">>")?;
        match name {
            "monitoradded" => {
                let id = data.to_string();
                Some(CompositorEvent::OutputAdded(id))
            }
            "monitorremoved" => {
                let id = data.to_string();
                Some(CompositorEvent::OutputRemoved(id))
            }
            "focusedmon" => {
                let (o_id, w_id) = data.split_once(',')?;
                let w_id: i64 = w_id.parse().ok()?;
                Some(CompositorEvent::OutputFocused(o_id.to_string(), w_id))
            }

            "workspace" => {
                let id: i64 = data.parse().ok()?;
                Some(CompositorEvent::ActiveWorkspace(id))
            }
            "createworkspacev2" => {
                let (id, _) = data.split_once(',')?;
                let id: i64 = id.parse().ok()?;
                Some(CompositorEvent::WorkspaceCreated(id))
            }
            "destroyworkspacev2" => {
                let (id, _) = data.split_once(',')?;
                let id: i64 = id.parse().ok()?;
                Some(CompositorEvent::WorkspaceDestroyed(id))
            }
            "moveworkspace" => {
                let (w_id, o_id) = data.split_once(',')?;
                let w_id: i64 = w_id.parse().ok()?;
                Some(CompositorEvent::WorkspaceMoved(w_id, o_id.to_string()))
            }

            "openwindow" => {
                let id = data.split_once(',')?.0;
                let id = id.strip_prefix("0x").unwrap_or(id);
                Some(CompositorEvent::OpenToplevel(id.to_string()))
            }
            "activewindowv2" => {
                let data = data.strip_prefix("0x").unwrap_or(data);
                Some(CompositorEvent::ActiveToplevel(data.to_string()))
            }
            "closewindow" => {
                let data = data.strip_prefix("0x").unwrap_or(data);
                Some(CompositorEvent::CloseToplevel(data.to_string()))
            }
            "movewindowv2" => {
                let (t_id, w_id) = data.split_once(',')?;
                let w_id = w_id.split_once(',')?.0;
                let w_id: i64 = w_id.parse().ok()?;
                Some(CompositorEvent::ToplevelMoved(t_id.to_string(), w_id))
            }
            "fullscreen" => match data {
                "1" => Some(CompositorEvent::Fullscreen(true)),
                "0" => Some(CompositorEvent::Fullscreen(false)),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn occlusion_snapshot<C: Compositor>(compositor: C) -> anyhow::Result<OcclusionTracker<C>> {
        let clients = Self::clients(compositor.request_socket_path())?;
        let outputs = compositor.outputs()?;
        let workspaces = compositor.workspaces()?;
        let active_toplevel = compositor.active_toplevel()?;
        let active_output = outputs
            .iter()
            .find(|o| o.3)
            .context("Failed to find active_output")?;

        OcclusionTracker::new(
            compositor,
            active_output.1.clone(),
            active_output.2, // active workspace
            active_toplevel,
            clients.as_slice(),
            workspaces.as_slice(),
            outputs.as_slice(),
        )
    }

    fn clients(request_socket_path: &Path) -> anyhow::Result<Vec<Client>> {
        let v = request_json(request_socket_path, b"j/clients", Framing::UntilEof)?;
        v.as_array()
            .context("Hyprland clients response is not an array")?
            .iter()
            .map(|client| -> anyhow::Result<_> {
                let address = client["address"]
                    .as_str()
                    .context("missing client address")?;
                let address = address.strip_prefix("0x").unwrap_or(address);
                let fullscreen = client["fullscreen"]
                    .as_u64()
                    .context("missing client fullscreen")?;
                let is_fullscreen = fullscreen >= 2;
                let workspace = client["workspace"]["id"]
                    .as_i64()
                    .context("missing client workspace id")?;
                let output = client["monitor"]
                    .as_i64()
                    .context("missing client monitor")?;

                Ok((address.to_string(), is_fullscreen, workspace, output))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }
}

impl Compositor for Hyprland {
    fn request_socket_path(&self) -> &std::path::Path {
        &self.request_socket_path
    }

    fn event_socket_path(&self) -> &std::path::Path {
        &self.event_socket_path
    }

    fn reader_loop(
        &self,
        tx: calloop::channel::Sender<ReadEvent>,
        shutdown: Arc<AtomicBool>,
        stream: Arc<Mutex<Option<UnixStream>>>,
    ) {
        while !shutdown.load(Ordering::Acquire) {
            match UnixStream::connect(&self.event_socket_path) {
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

                    let mut occlusion_tracker = match Self::occlusion_snapshot(self.clone()) {
                        Ok(snapshot) => {
                            tracing::info!("{:?}", snapshot);
                            snapshot
                        }
                        Err(e) => {
                            *stream.lock().unwrap() = None;
                            drop(stream);
                            tracing::error!("Hyprland initial state snapshot failed: {e:#}");
                            // ignore error since the main loop must close anyway
                            let _ = tx.send(ReadEvent::ReaderKilled(()));
                            break;
                        }
                    };

                    if tx
                        .send(ReadEvent::OutputStatus(
                            occlusion_tracker.output_status_snapshot(),
                        ))
                        .is_err()
                    {
                        *stream.lock().unwrap() = None;
                        return; // main loop dead
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
                        if let Some(event) = Self::parse_event(trimmed) {
                            if let Err(e) = occlusion_tracker.handle_event(&event) {
                                tracing::error!("{e:#}");
                                *stream.lock().unwrap() = None;
                                wait_for_shutdown(&shutdown);
                                break;
                            }
                            if tx
                                .send(ReadEvent::CompositorEvent(CompositorEvent::event_type(
                                    &event,
                                )))
                                .is_err()
                            {
                                *stream.lock().unwrap() = None;
                                return; // main loop dead
                            }
                            for change in occlusion_tracker.get_output_status().iter().flatten() {
                                if tx
                                    .send(ReadEvent::OutputStatusUpdated(change.clone()))
                                    .is_err()
                                {
                                    *stream.lock().unwrap() = None;
                                    return; // main loop dead
                                }
                            }
                        }
                    }

                    *stream.lock().unwrap() = None;
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    wait_for_shutdown(&shutdown);
                    tracing::warn!("Hyprland connection closed, reconnecting...");
                }
                Err(_) => {
                    wait_for_shutdown(&shutdown);
                    tracing::error!("Hyprland connection error, reconnecting...");
                }
            }
        }
    }

    fn active_workspace(&self) -> anyhow::Result<WorkspaceId> {
        let v = request_json(
            self.request_socket_path(),
            b"j/activeworkspace",
            Framing::UntilEof,
        )?;
        v["id"].as_i64().context("missing workspace id")
    }

    fn active_output(&self) -> anyhow::Result<String> {
        let v = request_json(&self.request_socket_path, b"j/monitors", Framing::UntilEof)?;
        let active_monitor = v
            .as_array()
            .context("Hyprland monitors response is not an array")?
            .iter()
            .find(|m| m["focused"].as_bool().unwrap_or_default())
            .context("Active output not found")?;

        Ok(active_monitor["name"]
            .as_str()
            .context("missing monitor name")?
            .to_string())
    }

    fn active_toplevel(&self) -> anyhow::Result<Option<ToplevelId>> {
        let v = request_json(
            &self.request_socket_path,
            b"j/activewindow",
            Framing::UntilEof,
        )?;
        let Some(address) = v["address"].as_str() else {
            return Ok(None);
        };

        Ok(Some(
            address.strip_prefix("0x").unwrap_or(address).to_string(),
        ))
    }

    fn outputs(&self) -> anyhow::Result<Vec<(i64, String, WorkspaceId, bool)>> {
        let v = request_json(&self.request_socket_path, b"j/monitors", Framing::UntilEof)?;
        v.as_array()
            .context("Hyprland monitors response is not an array")?
            .iter()
            .map(|m| {
                let id = m["id"].as_i64().context("missing output id")?;
                let name = m["name"].as_str().context("missing output name")?;
                let focused = m["focused"].as_bool().context("missing output focused")?;
                let acitve_workspace = m["activeWorkspace"]["id"]
                    .as_i64()
                    .context("missing output active workspace id")?;
                Ok((id, name.to_string(), acitve_workspace, focused))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }

    fn workspaces(&self) -> anyhow::Result<Vec<(WorkspaceId, OutputId)>> {
        let v = request_json(
            self.request_socket_path(),
            b"j/workspaces",
            Framing::UntilEof,
        )?;
        v.as_array()
            .context("Hyprland monitors response is not an array")?
            .iter()
            .map(|m| {
                let id = m["id"].as_i64().context("missing workspace id")?;
                let output = m["monitor"]
                    .as_str()
                    .context("missing workspace output name")?;
                Ok((id, output.to_string()))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::{CompositorEventKind, CompositorEventReader, ReadEvent};

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

    #[test]
    fn parses_workspace_event() {
        assert!(matches!(
            Hyprland::parse_event("workspace>>3"),
            Some(CompositorEvent::ActiveWorkspace(3))
        ));
    }

    #[test]
    fn parses_monitor_added_event() {
        assert!(matches!(
            Hyprland::parse_event("monitoradded>>eDP-1"),
            Some(CompositorEvent::OutputAdded(ref id)) if id == "eDP-1"
        ));
    }

    #[test]
    fn parses_monitor_removed_event() {
        assert!(matches!(
            Hyprland::parse_event("monitorremoved>>eDP-1"),
            Some(CompositorEvent::OutputRemoved(ref id)) if id == "eDP-1"
        ));
    }

    #[test]
    fn parses_focused_monitor_event() {
        assert!(matches!(
            Hyprland::parse_event("focusedmon>>eDP-1,3"),
            Some(CompositorEvent::OutputFocused(ref id, 3)) if id == "eDP-1"
        ));
    }

    #[test]
    fn parses_create_workspace_v2_event() {
        assert!(matches!(
            Hyprland::parse_event("createworkspacev2>>-98,special:hidden:2"),
            Some(CompositorEvent::WorkspaceCreated(-98))
        ));
    }

    #[test]
    fn parses_destroy_workspace_v2_event() {
        assert!(matches!(
            Hyprland::parse_event("destroyworkspacev2>>-98,special:hidden:2"),
            Some(CompositorEvent::WorkspaceDestroyed(-98))
        ));
    }

    #[test]
    fn parses_move_workspace_event() {
        assert!(matches!(
            Hyprland::parse_event("moveworkspace>>3,eDP-1"),
            Some(CompositorEvent::WorkspaceMoved(3, ref id)) if id == "eDP-1"
        ));
    }

    #[test]
    fn parses_open_window_event() {
        assert!(matches!(
            Hyprland::parse_event("openwindow>>0x1234,1"),
            Some(CompositorEvent::OpenToplevel(ref id)) if id == "1234"
        ));
    }

    #[test]
    fn parses_active_window_v2_event() {
        assert!(matches!(
            Hyprland::parse_event("activewindowv2>>55952ac54350"),
            Some(CompositorEvent::ActiveToplevel(ref id)) if id == "55952ac54350"
        ));
    }

    #[test]
    fn parses_close_window_event() {
        assert!(matches!(
            Hyprland::parse_event("closewindow>>0x1234"),
            Some(CompositorEvent::CloseToplevel(ref id)) if id == "1234"
        ));
    }

    #[test]
    fn parses_move_window_v2_event() {
        assert!(matches!(
            Hyprland::parse_event("movewindowv2>>55952ac54350,-98,special:hidden:2"),
            Some(CompositorEvent::ToplevelMoved(ref id, -98)) if id == "55952ac54350"
        ));
    }

    #[test]
    fn parses_fullscreen_event() {
        assert!(matches!(
            Hyprland::parse_event("fullscreen>>1"),
            Some(CompositorEvent::Fullscreen(true))
        ));
        assert!(matches!(
            Hyprland::parse_event("fullscreen>>0"),
            Some(CompositorEvent::Fullscreen(false))
        ));
    }

    #[test]
    fn ignores_unknown_event() {
        assert!(Hyprland::parse_event("monitoradded").is_none());
        assert!(Hyprland::parse_event("fullscreen>>2").is_none());
    }

    #[test]
    fn ignores_malformed_event() {
        assert!(Hyprland::parse_event("no-separator-here").is_none());
        assert!(Hyprland::parse_event("workspace>>abc").is_none());
        assert!(Hyprland::parse_event("focusedmon>>eDP-1").is_none());
        assert!(Hyprland::parse_event("movewindowv2>>0x1,3").is_none());
    }

    #[test]
    #[ignore = "requires active Hyprland session"]
    fn active_workspace_request_returns_workspace_id() {
        if !hyprland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let id = hyprland
            .active_workspace()
            .expect("active_workspace request failed");
        assert_ne!(id, 0, "active workspace id should not be 0");
    }

    #[test]
    #[ignore = "requires active Hyprland session"]
    fn active_output_request_returns_output_name() {
        if !hyprland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let name = hyprland
            .active_output()
            .expect("active_output request failed");
        assert!(!name.is_empty(), "active output name should not be empty");
    }

    #[test]
    #[ignore = "requires active Hyprland session"]
    fn active_toplevel_request_succeeds() {
        if !hyprland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let _toplevel = hyprland
            .active_toplevel()
            .expect("active_toplevel request failed");
    }

    #[test]
    #[ignore = "requires active Hyprland session"]
    fn outputs_request_returns_outputs() {
        if !hyprland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let outputs = hyprland.outputs().expect("outputs request failed");
        assert!(!outputs.is_empty(), "expected at least one monitor");
        assert!(
            outputs.iter().filter(|o| o.3).count() <= 1,
            "at most one monitor should be focused"
        );
        for (_, name, _, _) in &outputs {
            assert!(!name.is_empty(), "monitor name should not be empty");
        }
    }

    #[test]
    #[ignore = "requires active Hyprland session"]
    fn workspaces_request_returns_workspaces() {
        if !hyprland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let workspaces = hyprland.workspaces().expect("workspaces request failed");
        assert!(!workspaces.is_empty(), "expected at least one workspace");
        for (_, output) in &workspaces {
            assert!(!output.is_empty(), "workspace output should not be empty");
        }
    }

    #[test]
    #[ignore = "requires user interaction"]
    fn workspace_changed_event_reaches_subscribed_handler() {
        if !hyprland_env_available() || !wayland_env_available() {
            return;
        }

        let hyprland = Hyprland::connect().expect("Hyprland::connect() failed");
        let mut event_loop: calloop::EventLoop<'static, crate::app::state::App> =
            calloop::EventLoop::try_new().expect("failed to create event loop");
        let mut app = build_test_app(event_loop.handle()).expect("failed to build test App");

        let mut compositor_event_reader = CompositorEventReader::new();
        compositor_event_reader.add_dispatcher(Box::new(move |app, event| {
            if let ReadEvent::CompositorEvent(e) = event
                && *e == CompositorEventKind::ActiveWorkspace
            {
                app.stop_main_loop();
            }
        }));
        let (tx, rx) = calloop::channel::channel::<ReadEvent>();

        app.main_loop_signal = Some(event_loop.get_signal());

        let compositor_event_reader =
            std::rc::Rc::new(std::cell::RefCell::new(compositor_event_reader));
        let compositor_event_reader_for_spawn = compositor_event_reader.clone();
        let mut tx = Some(tx);
        event_loop
            .handle()
            .insert_source(calloop::timer::Timer::immediate(), move |_, _, app| {
                if let Some(tx) = tx.take()
                    && let Err(e) = compositor_event_reader_for_spawn
                        .borrow_mut()
                        .spawn(tx, hyprland.clone())
                {
                    eprintln!("{e:#}");
                    app.stop_main_loop();
                }
                calloop::timer::TimeoutAction::Drop
            })
            .expect("failed to insert spawn source");

        event_loop
            .handle()
            .insert_source(rx, move |event, _, state| {
                if let calloop::channel::Event::Msg(event) = event {
                    compositor_event_reader.borrow().dispatch(&event, state);
                }
            })
            .expect("failed to insert channel source");

        event_loop
            .handle()
            .insert_source(
                calloop::timer::Timer::from_duration(std::time::Duration::from_secs(10)),
                move |_, _, app| {
                    eprintln!("timeout: no ActiveWorkspace event received");
                    app.stop_main_loop();
                    calloop::timer::TimeoutAction::Drop
                },
            )
            .expect("failed to insert timeout timer");

        println!("Switch workspace now. Waiting 10s...");
        let _ = std::io::Write::flush(&mut std::io::stdout());

        event_loop
            .run(None, &mut app, |_| {})
            .expect("event loop failed");

        drop(app);
    }

    fn build_test_app(
        loop_handle: calloop::LoopHandle<'static, crate::app::state::App>,
    ) -> anyhow::Result<crate::app::state::App> {
        let (conn, globals, queue) = crate::wayland::connection::WlConnection::connect()?;
        let (compositor, layer_shell) =
            crate::wayland::connection::WlConnection::bind_required_protocol_extensions(
                &globals, &queue,
            )?;
        let wl_conn = crate::wayland::connection::WlConnection::new(
            conn,
            globals,
            queue.handle(),
            compositor,
            layer_shell,
        );

        let mut app = crate::app::state::App::new(
            wl_conn,
            crate::video::decoder::Decoder::new(),
            crate::video::timing::Timing::new(),
        );
        app.occlusion = Some(crate::app::state::OcclusionRuntime::new(loop_handle));
        Ok(app)
    }
}
