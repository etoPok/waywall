use std::ffi::CString;
use std::os::raw::c_void;
use std::path::Path;

use anyhow::{Context, Ok, Result};
use calloop::ping;
use tracing::{info, warn};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{globals::registry_queue_init, Connection};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::cli::args::Args;
use crate::decoder::Decoder;
use crate::render::egl::init_egl;
use crate::render::state::RenderState;
use crate::shader::{QuadGeometry, Shader};

use super::state::{App, Monitor};

pub struct BootstrapOutput {
    pub app: App,
    pub conn: Connection,
    pub queue: wayland_client::EventQueue<App>,
    pub ping_source: calloop::ping::PingSource,
    pub error_ping_source: calloop::ping::PingSource,
}

pub fn bootstrap(args: &mut Args) -> Result<BootstrapOutput> {
    info!("waywall starting with video: {}", &args.video_path);

    // Validate file
    let video_path = Path::new(&args.video_path);
    if !video_path.exists() {
        anyhow::bail!("Video file does not exist: {}", video_path.display());
    }
    let video_path = video_path
        .canonicalize()
        .context("Error resolving video path")?;
    let video_path_str = video_path.to_string_lossy().to_string();

    // ------------------------------------------------------------------
    // Connect to Wayland
    // ------------------------------------------------------------------

    let conn = Connection::connect_to_env()
        .context("Could not connect to Wayland server, is WAYLAND_DISPLAY set?")?;

    let wl_display_ptr = conn.backend().display_ptr() as *mut c_void;

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

    if args.use_vaapi {
        let dmabuf: Option<ZwpLinuxDmabufV1> = globals.bind(&qh, 1..=4, ()).ok();
        if dmabuf.is_none() {
            warn!("zwp_linux_dmabuf_v1 not availble, zero-copy dmabuf path disabled. Using software fallback");
            args.use_vaapi = false;
            args.use_gl = true;
        }
    }

    // ------------------------------------------------------------------
    // Initial state
    // ------------------------------------------------------------------

    let mut app = App::new(conn.clone(), compositor, layer_shell);
    app.qh = Some(qh.clone());
    app.viewporter = viewporter;

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

    // Filter by output name if requested
    if !args.outputs.is_empty() {
        let invalid_names: Vec<&String> = args
            .outputs
            .iter()
            .filter(|out| {
                !app.monitors
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

        app.monitors.retain(|m| {
            m.name
                .as_deref()
                .is_some_and(|name| args.outputs.iter().any(|out| out == name))
        });
    }

    if app.monitors.is_empty() {
        anyhow::bail!(
            "No outputs detected. Make sure output names match. \
             Requested: {:?}",
            args.outputs
        );
    }

    // ------------------------------------------------------------------
    // Create layer-shell surfaces
    // ------------------------------------------------------------------

    for (i, monitor) in app.monitors.iter_mut().enumerate() {
        if monitor.physical_width == 0 || monitor.physical_height == 0 {
            warn!("Output dimensions not detected, using 1920x1080 as fallback");
            monitor.physical_width = 1920;
            monitor.physical_height = 1080;
        }

        App::create_surfaces(
            &app.compositor,
            &app.layer_shell,
            app.viewporter.as_ref(),
            &qh,
            monitor,
            i,
        );
    }

    let mut configure_attempts = 0;
    while !app.configured && configure_attempts < 50 {
        queue
            .blocking_dispatch(&mut app)
            .context("Error waiting for configure")?;
        configure_attempts += 1;
    }

    if !app.configured {
        anyhow::bail!(
            "Compositor did not send configuration after {} attempts.",
            configure_attempts
        );
    }

    if args.use_gl {
        initialize_gl_egl(&mut app, wl_display_ptr)?;
    }

    // ------------------------------------------------------------------
    // Decoder PingSource
    // ------------------------------------------------------------------

    let (ping, ping_source) = ping::make_ping().context("Failed to create decoder wakeup ping")?;
    let notifier = crate::notifier::Notifier(ping);

    let (error_ping, error_ping_source) =
        ping::make_ping().context("Failed to create decoder error ping")?;

    // ------------------------------------------------------------------
    // Start decoder
    // ------------------------------------------------------------------

    let decoder = Decoder::start(
        &video_path_str,
        app.frame_queue.clone(),
        notifier,
        error_ping,
        args.use_vaapi,
    )
    .context("Failed to start decoder")?;

    info!("Decoder started: time_base={}", decoder.time_base);

    app.decoder = Some(decoder);

    Ok(BootstrapOutput {
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
    })
}

pub fn bootstrap_drm_pipeline(args: &mut Args) -> Result<BootstrapOutput> {
    info!("waywall starting with video: {}", &args.video_path);

    // Validate file
    let video_path = Path::new(&args.video_path);
    if !video_path.exists() {
        anyhow::bail!("Video file does not exist: {}", video_path.display());
    }

    // ------------------------------------------------------------------
    // Connect to Wayland
    // ------------------------------------------------------------------

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

    let mut dmabuf: Option<ZwpLinuxDmabufV1> = None;
    if args.use_vaapi {
        dmabuf = globals.bind(&qh, 1..=3, ()).ok();
        if dmabuf.is_none() {
            warn!("zwp_linux_dmabuf_v1 not availble, zero-copy dmabuf path disabled. Using software fallback");
            args.use_vaapi = false;
            args.use_gl = true;
        }
    }

    // ------------------------------------------------------------------
    // Initial state
    // ------------------------------------------------------------------

    let mut app = App::new(conn.clone(), compositor, layer_shell);
    app.qh = Some(qh.clone());
    app.viewporter = viewporter;
    app.dmabuf = dmabuf;

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

    // Filter by output name if requested
    if !args.outputs.is_empty() {
        let invalid_names: Vec<&String> = args
            .outputs
            .iter()
            .filter(|out| {
                !app.monitors
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

        app.monitors.retain(|m| {
            m.name
                .as_deref()
                .is_some_and(|name| args.outputs.iter().any(|out| out == name))
        });
    }

    if app.monitors.is_empty() {
        anyhow::bail!(
            "No outputs detected. Make sure output names match. \
             Requested: {:?}",
            args.outputs
        );
    }

    // ------------------------------------------------------------------
    // Create layer-shell surfaces
    // ------------------------------------------------------------------

    for (i, monitor) in app.monitors.iter_mut().enumerate() {
        if monitor.physical_width == 0 || monitor.physical_height == 0 {
            warn!("Output dimensions not detected, using 1920x1080 as fallback");
            monitor.physical_width = 1920;
            monitor.physical_height = 1080;
        }

        App::create_surfaces(
            &app.compositor,
            &app.layer_shell,
            app.viewporter.as_ref(),
            &qh,
            monitor,
            i,
        );
    }

    let mut configure_attempts = 0;
    while !app.configured && configure_attempts < 50 {
        queue
            .blocking_dispatch(&mut app)
            .context("Error waiting for configure")?;
        configure_attempts += 1;
    }

    if !app.configured {
        anyhow::bail!(
            "Compositor did not send configuration after {} attempts.",
            configure_attempts
        );
    }

    // ------------------------------------------------------------------
    // Decoder PingSource
    // ------------------------------------------------------------------

    let (ping, ping_source) = ping::make_ping().context("Failed to create decoder wakeup ping")?;
    let notifier = crate::notifier::Notifier(ping);

    let (error_ping, error_ping_source) =
        ping::make_ping().context("Failed to create decoder error ping")?;

    // ------------------------------------------------------------------
    // Start decoder
    // ------------------------------------------------------------------

    let decoder = Decoder::start(
        video_path.to_string_lossy().as_ref(),
        app.frame_queue.clone(),
        notifier,
        error_ping,
        args.use_vaapi,
    )
    .context("Failed to start decoder")?;

    info!("Decoder started: time_base={}", decoder.time_base);

    app.decoder = Some(decoder);

    Ok(BootstrapOutput {
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
    })
}

fn initialize_gl_egl(app: &mut App, wl_display_ptr: *mut c_void) -> Result<(), anyhow::Error> {
    for monitor in app.monitors.iter() {
        if monitor.wl_surface_ptr.is_null() {
            anyhow::bail!("Could not obtain the native pointer of the wl_surface");
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

        let (egl_display, egl_surface, egl_context, egl_window) = unsafe {
            init_egl(wl_display_ptr, wl_surface_ptr, width, height)
                .context("Error initializing EGL")?
        };

        app.render_states.push(RenderState {
            egl_display,
            egl_surface,
            egl_context,
            egl_window,
            width,
            height,
            textures: Vec::new(),
        });
    }

    {
        let last_rs = app.render_states.last().unwrap();
        unsafe {
            crate::render::egl::eglMakeCurrent(
                last_rs.egl_display,
                last_rs.egl_surface,
                last_rs.egl_surface,
                last_rs.egl_context,
            );
        }
    }

    gl::load_with(|name| {
        let c_str = CString::new(name).unwrap();
        unsafe { crate::render::egl::eglGetProcAddress(c_str.as_ptr()) as *const _ }
    });

    info!("OpenGL functions loaded successfully");

    let shader_yuv = Shader::new_yuv420p();
    let shader_nv12 = Shader::new_nv12();
    info!("Shaders compiled (YUV420P + NV12)");

    let quad = QuadGeometry::new();
    info!("Quad geometry initialized");

    app.shader_yuv = Some(shader_yuv);
    app.shader_nv12 = Some(shader_nv12);
    app.quad = Some(quad);

    Ok(())
}
