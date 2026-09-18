use std::ffi::CString;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Ok, Result, bail};
use calloop::ping::{self, PingSource};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use tracing::{debug, warn};
use wayland_backend::client::WaylandError;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{Connection, EventQueue, QueueHandle, globals::registry_queue_init};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::cli::args::Args;
use crate::decoder::Decoder;
use crate::drm_node::render_node_from_main_device;
use crate::frame_queue::FrameQueue;
use crate::render::egl::{
    create_egl_ctx, create_egl_surface, eglDestroyContext, eglDestroySurface, eglMakeCurrent,
    eglSwapInterval, init_egl_display, wl_egl_window_destroy,
};
use crate::render::state::{GlContext, RenderState};
use crate::timing::Timing;
use crate::wayland::surfaces::create_surface;

use super::state::{App, Monitor};

pub struct BootstrapOutput {
    pub app: App,
    pub conn: Connection,
    pub queue: wayland_client::EventQueue<App>,
    pub ping_source: calloop::ping::PingSource,
    pub error_ping_source: calloop::ping::PingSource,
}

pub fn bootstrap(args: &mut Args) -> Result<BootstrapOutput> {
    if args.use_hwdec {
        bootstrap_drm(args)
    } else {
        bootstrap_gl_egl(args)
    }
}

pub fn bootstrap_gl_egl(args: &mut Args) -> Result<BootstrapOutput> {
    let video_path = canonize_video_path(&args.video_path)?;

    let conn = Connection::connect_to_env()
        .context("Could not connect to Wayland server, is WAYLAND_DISPLAY set?")?;

    let (globals, mut queue) =
        registry_queue_init::<App>(&conn).context("Error initializing Wayland registry")?;
    let qh = queue.handle();

    let compositor = globals
        .bind(&qh, 4..=5, ())
        .context("Compositor does not support wl_compositor")?;

    let layer_shell = globals
        .bind(&qh, 1..=4, ())
        .context("Compositor does not support zwlr_layer_shell_v1")?;

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    if viewporter.is_none() {
        warn!("wl_viewporter not available, fallback to logical size for EGL");
    }

    let mut app = App::new(
        conn.clone(),
        qh.clone(),
        compositor,
        layer_shell,
        conn.backend().display_ptr() as *mut c_void,
        viewporter,
        None,
        Decoder::new(),
        Timing::new(),
    );

    let registry = globals.registry();
    for global in globals.contents().clone_list() {
        if global.interface == "wl_output" {
            let output =
                registry.bind::<WlOutput, _, _>(global.name, global.version.min(4), &qh, ());
            app.monitors.push(Monitor::new(output));
        }
    }

    queue
        .roundtrip(&mut app)
        .context("Error in initial roundtrip")?;

    filter_outputs_by_name(&mut app.monitors, &args.outputs)?;
    create_layer_shell_surfaces(&mut app, &qh);

    wait_for_wayland_events_state(
        &conn,
        &mut queue,
        &mut app,
        Duration::from_secs(2),
        |state| state.configured,
    )?;

    if !app.configured {
        bail!("Compositor did not send zwl_layer_surface_v1.configure within waiting time.");
    }

    initialize_gl_egl(&mut app)?;

    let (ping_source, error_ping_source) = start_decoder(
        &mut app.decoder,
        &mut app.timing,
        &video_path,
        &app.frame_queue,
        None,
    )?;

    Ok(BootstrapOutput {
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
    })
}

pub fn bootstrap_drm(args: &mut Args) -> Result<BootstrapOutput> {
    let video_path = canonize_video_path(&args.video_path)?;

    let conn = Connection::connect_to_env()
        .context("Could not connect to Wayland server, is WAYLAND_DISPLAY set?")?;

    let (globals, mut queue) =
        registry_queue_init::<App>(&conn).context("Error initializing Wayland registry")?;
    let qh = queue.handle();

    let compositor = globals
        .bind(&qh, 4..=5, ())
        .context("Compositor does not support wl_compositor")?;

    let layer_shell = globals
        .bind(&qh, 1..=4, ())
        .context("Compositor does not support zwlr_layer_shell_v1")?;

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    if viewporter.is_none() {
        warn!("wl_viewporter not available, fallback to logical size for EGL");
    }

    let dmabuf: ZwpLinuxDmabufV1 = globals.bind(&qh, 4..=4, ()).context(
        "Compositor does not support zwp_linux_dmabuf_v1 v4 (required for drm pipeline)",
    )?;
    let feedback = dmabuf.get_default_feedback(&qh, ());

    let mut app = App::new(
        conn.clone(),
        qh.clone(),
        compositor,
        layer_shell,
        std::ptr::null_mut::<c_void>(),
        viewporter,
        Some(dmabuf),
        Decoder::new(),
        Timing::new(),
    );

    let registry = globals.registry();
    for global in globals.contents().clone_list() {
        if global.interface == "wl_output" {
            let output =
                registry.bind::<WlOutput, _, _>(global.name, global.version.min(4), &qh, ());
            app.monitors.push(Monitor::new(output));
        }
    }

    queue
        .roundtrip(&mut app)
        .context("Error in initial roundtrip")?;

    filter_outputs_by_name(&mut app.monitors, &args.outputs)?;
    create_layer_shell_surfaces(&mut app, &qh);

    wait_for_wayland_events_state(
        &conn,
        &mut queue,
        &mut app,
        Duration::from_secs(2),
        |state| state.configured && state.dmabuf_main_device.is_some(),
    )?;

    if !app.configured {
        feedback.destroy();
        bail!("Compositor did not send zwl_layer_surface_v1.configure within waiting time.");
    }

    if app.dmabuf_main_device.is_none() {
        feedback.destroy();
        bail!(
            "Compositor did not send zwp_linux_dmabuf_feedback_v1.main_device within waiting time."
        );
    }

    feedback.destroy();

    let render_node = if args.use_hwdec {
        let rn = render_node_from_main_device(app.dmabuf_main_device.as_ref().unwrap())
            .context("VAAPI requested but no DRM render node was resolved from the compositor")?;
        debug!("Render node: {}", rn.display());
        Some(rn)
    } else {
        None
    };

    let (ping_source, error_ping_source) = start_decoder(
        &mut app.decoder,
        &mut app.timing,
        &video_path,
        &app.frame_queue,
        render_node,
    )?;

    Ok(BootstrapOutput {
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
    })
}

fn canonize_video_path(video_path: &str) -> Result<String> {
    let path = Path::new(video_path);
    if !path.exists() {
        bail!("Video path does not exist: {}", video_path);
    }

    Ok(path
        .canonicalize()
        .context("Error resolving video path")?
        .to_string_lossy()
        .into_owned())
}

fn start_decoder(
    decoder: &mut Decoder,
    timing: &mut Timing,
    video_path: &str,
    frame_queue: &Arc<FrameQueue>,
    render_node: Option<PathBuf>,
) -> Result<(PingSource, PingSource)> {
    let (ping, ping_source) = ping::make_ping().context("Failed to create decoder wakeup ping")?;
    let (error_ping, error_ping_source) =
        ping::make_ping().context("Failed to create decoder error ping")?;

    decoder
        .start(
            video_path,
            frame_queue.clone(),
            ping,
            error_ping,
            render_node.as_deref(),
        )
        .context("Failed to start decoder")?;
    timing.configure(decoder.time_base);

    Ok((ping_source, error_ping_source))
}

fn initialize_gl_egl(app: &mut App) -> Result<(), anyhow::Error> {
    let (egl_display, egl_config) = unsafe { init_egl_display(app.wl_display)? };
    let mut egl_ctx = std::ptr::null_mut::<c_void>();
    let mut egl_make_current = true;

    for monitor in app.monitors.iter() {
        if monitor.wl_surface_ptr.is_null() {
            bail!("Could not obtain the native pointer of the wl_surface");
        }

        let wl_surface_ptr = monitor.wl_surface_ptr;

        let width = if monitor.physical_width > 0 {
            monitor.physical_width
        } else {
            monitor.logical_width.max(1920)
        } as i32;
        let height = if monitor.physical_height > 0 {
            monitor.physical_height
        } else {
            monitor.logical_height.max(1080)
        } as i32;

        let (egl_surface, egl_window) =
            unsafe { create_egl_surface(egl_display, wl_surface_ptr, egl_config, width, height)? };

        // check eglMakeCurrent with the first available RenderState to avoid resource creation and
        // destruction
        if egl_make_current {
            unsafe {
                egl_ctx = create_egl_ctx(egl_display, egl_config)?;
                if eglMakeCurrent(egl_display, egl_surface, egl_surface, egl_ctx) == 0 {
                    wl_egl_window_destroy(egl_window);
                    eglDestroySurface(egl_display, egl_surface);
                    eglDestroyContext(egl_display, egl_ctx);
                    bail!("eglMakeCurrent failed");
                }
                eglSwapInterval(egl_display, 0);
            }
            egl_make_current = false;
        }

        app.render_states.push(RenderState {
            egl_display,
            egl_surface,
            egl_window,
            width,
            height,
        });
    }

    gl::load_with(|name| {
        let c_str = CString::new(name).unwrap();
        unsafe { crate::render::egl::eglGetProcAddress(c_str.as_ptr()) as *const _ }
    });
    debug!("OpenGL functions loaded successfully");

    app.gl_ctx = Some(GlContext::new(egl_display, egl_ctx));

    Ok(())
}

fn filter_outputs_by_name(monitors: &mut Vec<Monitor>, requested_outputs: &[String]) -> Result<()> {
    if !requested_outputs.is_empty() {
        let invalid_names: Vec<&String> = requested_outputs
            .iter()
            .filter(|out| {
                !monitors
                    .iter()
                    .any(|m| m.name.as_deref().is_some_and(|name| name == *out))
            })
            .collect();

        if !invalid_names.is_empty() {
            anyhow::bail!(
                "The following output names do not exist: {:?}",
                invalid_names
            );
        }

        monitors.retain(|m| {
            m.name
                .as_deref()
                .is_some_and(|name| requested_outputs.iter().any(|out| out == name))
        });
    }

    if monitors.is_empty() {
        anyhow::bail!(
            "No outputs detected. Make sure output names match. \
             Requested: {:?}",
            requested_outputs
        );
    }

    Ok(())
}

fn create_layer_shell_surfaces(app: &mut App, qh: &QueueHandle<App>) {
    let (compositor, layer_shell, viewporter, monitors) = (
        &app.compositor,
        &app.layer_shell,
        app.viewporter.as_ref(),
        &mut app.monitors,
    );

    for (i, monitor) in monitors.iter_mut().enumerate() {
        if monitor.physical_width == 0 || monitor.physical_height == 0 {
            warn!("Output dimensions not detected, using 1920x1080 as fallback");
            monitor.physical_width = 1920;
            monitor.physical_height = 1080;
        }

        create_surface(compositor, layer_shell, viewporter, qh, monitor, i);
    }
}

pub fn wait_for_wayland_events_state<State, F>(
    _conn: &Connection,
    queue: &mut EventQueue<State>,
    state: &mut State,
    timeout: Duration,
    mut is_finished: F,
) -> anyhow::Result<()>
where
    F: FnMut(&State) -> bool,
{
    let deadline = Instant::now() + timeout;

    queue
        .flush()
        .context("Error flushing pending outgoing events")?;

    while !is_finished(state) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        let read_guard = match queue.prepare_read() {
            Some(guard) => guard,
            None => {
                queue
                    .dispatch_pending(state)
                    .context("Error in dispatch_pending")?;
                continue;
            }
        };

        // connection_fd() returns a BorrowedFd, which implements AsFd for nix.
        let fd = read_guard.connection_fd();
        let mut poll_fd = PollFd::new(fd, PollFlags::POLLIN);

        match poll(
            std::slice::from_mut(&mut poll_fd),
            PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX),
        ) {
            std::result::Result::Ok(0) => {
                continue;
            }
            std::result::Result::Ok(_) => {
                // Events are ready. Verify that the POLLIN flag is set (for safety).
                let revents = poll_fd.revents().unwrap_or_else(PollFlags::empty);
                if revents.contains(PollFlags::POLLIN) {
                    match read_guard.read() {
                        std::result::Result::Ok(_) => {}
                        Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // "Spurious wakeup": the kernel woke the thread but there is no real data yet. Safe to ignore.
                        }
                        Err(e) => anyhow::bail!("Error reading Wayland events: {}", e),
                    }
                }
            }
            Err(Errno::EINTR) => {
                // possible interrupt due to a system signal
                continue;
            }
            Err(e) => {
                anyhow::bail!("Error in Wayland poll: {}", e);
            }
        }

        queue
            .dispatch_pending(state)
            .context("Error in dispatch_pending after reading")?;
    }

    Ok(())
}
