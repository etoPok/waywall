use std::ffi::CString;
use std::os::raw::c_void;
use std::path::{Path, PathBuf};

use anyhow::{Context, Ok, Result, bail};
use calloop::ping::{self, PingSource};
use tracing::{debug, warn};
use wayland_client::EventQueue;

use super::state::{App, Monitor};
use crate::cli::args::Args;
use crate::decoder::Decoder;
use crate::drm_node::render_node_from_main_device;
use crate::render::egl::{
    create_egl_ctx, create_egl_surface, eglDestroyContext, eglDestroySurface, eglMakeCurrent,
    eglSwapInterval, init_egl_display, wl_egl_window_destroy,
};
use crate::render::state::{GlContext, RenderState};
use crate::timing::Timing;
use crate::wayland::connection::WlConnection;

pub struct BootstrapOutput {
    pub app: App,
    pub queue: EventQueue<App>,
    pub ping_source: calloop::ping::PingSource,
    pub error_ping_source: calloop::ping::PingSource,
}

pub fn bootstrap(args: &Args) -> Result<BootstrapOutput> {
    if args.use_hwdec {
        bootstrap_drm(args)
    } else {
        bootstrap_gl_egl(args)
    }
}

pub fn bootstrap_gl_egl(args: &Args) -> Result<BootstrapOutput> {
    let (mut app, mut queue) = common_connection()?;

    queue
        .roundtrip(&mut app)
        .context("Error in initial roundtrip")?;

    filter_outputs_by_name(&mut app.monitors, &args.outputs)?;
    create_layer_shell_surfaces(&mut app);

    app.wait_dispatch_events(&mut queue, std::time::Duration::from_secs(2), |_app| {
        _app.configured
    })?;

    if !app.configured {
        bail!("Compositor did not send zwl_layer_surface_v1.configure within waiting time.");
    }

    initialize_gl_egl(&mut app)?;

    let video_path = canonicalize_video_path(&args.video_path)?;
    let (ping_source, error_ping_source) = start_decoder(&mut app, &video_path, None)?;
    configure_timing(&app.decoder, &mut app.timing, args.fps);

    Ok(BootstrapOutput {
        app,
        queue,
        ping_source,
        error_ping_source,
    })
}

pub fn bootstrap_drm(args: &Args) -> Result<BootstrapOutput> {
    let (mut app, mut queue) = common_connection()?;
    app.wl_conn.bind_zwp_linux_dmabuf_v1()?;
    let feedback = app.wl_conn.get_default_feedback()?;

    queue
        .roundtrip(&mut app)
        .context("Error in initial roundtrip")?;

    filter_outputs_by_name(&mut app.monitors, &args.outputs)?;
    create_layer_shell_surfaces(&mut app);

    app.wait_dispatch_events(&mut queue, std::time::Duration::from_secs(2), |_app| {
        _app.configured && _app.wl_conn.main_device.is_some()
    })?;

    if !app.configured {
        feedback.destroy();
        bail!("Compositor did not send zwl_layer_surface_v1.configure within waiting time.");
    }

    if app.wl_conn.main_device.is_none() {
        feedback.destroy();
        bail!(
            "Compositor did not send zwp_linux_dmabuf_feedback_v1.main_device within waiting time."
        );
    }

    feedback.destroy();

    let render_node = if args.use_hwdec {
        let rn = render_node_from_main_device(app.wl_conn.main_device.as_ref().unwrap())
            .context("VAAPI requested but no DRM render node was resolved from the compositor")?;
        debug!("Render node: {}", rn.display());
        Some(rn)
    } else {
        None
    };

    let video_path = canonicalize_video_path(&args.video_path)?;
    let (ping_source, error_ping_source) = start_decoder(&mut app, &video_path, render_node)?;
    configure_timing(&app.decoder, &mut app.timing, args.fps);

    Ok(BootstrapOutput {
        app,
        queue,
        ping_source,
        error_ping_source,
    })
}

fn common_connection() -> Result<(App, EventQueue<App>)> {
    let (conn, globals, queue) = WlConnection::connect()?;
    let (compositor, layer_shell) =
        WlConnection::bind_required_protocol_extensions(&globals, &queue)?;
    let mut wl_conn = WlConnection::new(conn, globals, queue.handle(), compositor, layer_shell);
    wl_conn.bind_wp_viewporter()?;

    let mut app = App::new(wl_conn, Decoder::new(), Timing::new());
    app.monitors.extend(
        app.wl_conn
            .bind_all_available_wl_outputs()
            .into_iter()
            .map(Monitor::new),
    );

    Ok((app, queue))
}

fn configure_timing(decoder: &Decoder, timing: &mut Timing, fps: Option<u64>) {
    let avg_frame_rate = if decoder.avg_frame_rate.num > 0 && decoder.avg_frame_rate.den > 0 {
        decoder.avg_frame_rate.num as f64 / decoder.avg_frame_rate.den as f64
    } else {
        0.0
    };

    if let Some(f) = fps {
        let target_fps = f as f64;
        if avg_frame_rate <= 0.0 {
            timing.configure(decoder.time_base, 1.0);
            warn!(
                "Timing: invalid source frame rate ({:.2}). Playback capped at source frame rate.",
                avg_frame_rate
            );
            return;
        }

        if target_fps > avg_frame_rate {
            timing.configure(decoder.time_base, 1.0);
            warn!(
                "Timing: required --fps {} exceds the source frame rate. Playback capped at source frame rate ({:.2}).",
                f, avg_frame_rate
            );
            return;
        }

        let time_scale = target_fps / avg_frame_rate;
        timing.configure(decoder.time_base, time_scale);
        tracing::info!(
            "Timing: fps={}, source_fps={:.2}, time_scale={:.2}",
            f,
            avg_frame_rate,
            time_scale
        );
    } else {
        timing.configure(decoder.time_base, 1.0);
    }
}

fn canonicalize_video_path(video_path: &str) -> Result<String> {
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
    app: &mut App,
    video_path: &str,
    render_node: Option<PathBuf>,
) -> Result<(PingSource, PingSource)> {
    let (ping, ping_source) = ping::make_ping().context("Failed to create decoder wakeup ping")?;
    let (error_ping, error_ping_source) =
        ping::make_ping().context("Failed to create decoder error ping")?;

    app.decoder
        .start(
            video_path,
            app.frame_queue.clone(),
            ping,
            error_ping,
            render_node.as_deref(),
        )
        .context("Failed to start decoder")?;

    Ok((ping_source, error_ping_source))
}

fn initialize_gl_egl(app: &mut App) -> Result<(), anyhow::Error> {
    let wl_display = app.wl_conn.wl_display();
    let (egl_display, egl_config) = unsafe { init_egl_display(wl_display)? };
    let mut egl_ctx = std::ptr::null_mut::<c_void>();
    let mut egl_make_current = true;

    for monitor in app.monitors.iter() {
        let wl_surface_ptr = if monitor.wl_surface().is_null() {
            bail!("Could not obtain the native pointer of the wl_surface");
        } else {
            monitor.wl_surface()
        };

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

        app.render_states.push(RenderState::new(
            egl_display,
            egl_surface,
            egl_window,
            monitor.surface.as_ref().unwrap().clone(),
            width,
            height,
        ));
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

fn create_layer_shell_surfaces(app: &mut App) {
    let (compositor, layer_shell, viewporter) = (
        &app.wl_conn.compositor,
        &app.wl_conn.layer_shell,
        app.wl_conn.viewporter.as_ref(),
    );

    for (i, monitor) in app.monitors.iter_mut().enumerate() {
        if monitor.physical_width == 0 || monitor.physical_height == 0 {
            warn!("Output dimensions not detected, using 1920x1080 as fallback");
            monitor.physical_width = 1920;
            monitor.physical_height = 1080;
        }

        crate::wayland::surfaces::create_surface(
            compositor,
            layer_shell,
            viewporter,
            &app.wl_conn.qh,
            monitor,
            i,
        );
    }
}
