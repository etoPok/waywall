use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use calloop::ping;
use tracing::{info, warn};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::{globals::registry_queue_init, Connection};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::bindings::mpv::{
    mpv_get_property, mpv_node, mpv_render_context_set_update_callback, MPV_FORMAT_STRING,
};
use crate::cli::args::Args;
use crate::mpv::callbacks::mpv_update_callback;
use crate::mpv::events::fmt_mpv_error;
use crate::mpv::init::init_mpv;
use crate::render::egl::init_egl;
use crate::render::mpv_render::create_render_context;
use crate::render::state::RenderState;
use crate::runtime::wakeup::MpvUpdateState;

use super::state::{App, Monitor};

pub struct BootstrapOutput {
    pub app: App,
    pub conn: Connection,
    pub queue: wayland_client::EventQueue<App>,
    pub ping_source: ping::PingSource,
}

pub fn bootstrap(args: Args) -> Result<BootstrapOutput> {
    let video_path_str = args.video_path;

    info!(
        "mpvwall starting with video: {} (gpu-api: OpenGL)",
        video_path_str
    );

    // Validate that the video file exists before proceeding
    let video_path = Path::new(&video_path_str);
    if !video_path.exists() {
        anyhow::bail!("El archivo de video no existe: {}", video_path.display());
    }
    let video_path = video_path
        .canonicalize()
        .context("Error resolviendo la ruta del video")?;
    let video_path_str = video_path.to_string_lossy().to_string();

    // ------------------------------------------------------------------
    // Connect to Wayland
    // ------------------------------------------------------------------

    let conn = Connection::connect_to_env()
        .context("Coult not connect to Wayland server, is WAYLAND_DYSPLAY set?")?;

    let wl_display_ptr = { conn.backend().display_ptr() as *mut c_void };

    let (globals, mut queue) =
        registry_queue_init::<App>(&conn).context("Error initializing Wayland registry ")?;
    let qh = queue.handle();

    let compositor = globals
        .bind(&qh, 4..=5, ())
        .context("Compositor does not support wl_compositor")?;

    let layer_shell = globals
        .bind(&qh, 1..=4, ())
        .context("Composior does not support zwlr_layer_shell_v1")?;

    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    if viewporter.is_none() {
        warn!("wl_viewporter not available, fallback to logical size for EGL");
    }

    // ------------------------------------------------------------------
    // Initial state
    // ------------------------------------------------------------------

    let mut app = App::new(compositor, layer_shell);
    app.qh = Some(qh.clone());
    app.viewporter = viewporter;

    // Bind wl_output objects from the initial global list.
    // registry_queue_init already consumed the WlRegistry::Global events,
    // so we must iterate the stored GlobalListContents to discover outputs.
    let registry = globals.registry();
    for global in globals.contents().clone_list() {
        if global.interface == "wl_output" {
            let output =
                registry.bind::<WlOutput, _, _>(global.name, global.version.min(4), &qh, ());
            app.monitors.push(Monitor::new(output));
        }
    }
    info!("Detected {} output(s)", app.monitors.len());

    queue
        .roundtrip(&mut app)
        .context("Error in initial roundtrip")?;

    // validate that all passed output names exist among detected monitors
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
    // Create layer-shell surface
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

    // wait for surface config
    let mut configure_attempts = 0;
    while !app.configured && configure_attempts < 50 {
        // returns upon receiving events
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
    // Initialize EGL/OpenGL
    // ------------------------------------------------------------------

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
        });
    }

    // ------------------------------------------------------------------
    // Initialize libmpv
    // ------------------------------------------------------------------

    let mpv = init_mpv()?;

    // ------------------------------------------------------------------
    // Create mpv_render_context on the active EGLContext
    // ------------------------------------------------------------------

    let render_ctx =
        unsafe { create_render_context(&mpv).context("Error creating mpv_render_context")? };

    // ------------------------------------------------------------------
    // Set up mpv update callback + wakeup mechanism
    // ------------------------------------------------------------------

    let (ping, ping_source) = ping::make_ping().context("Error creating ping for wakeup")?;

    let update_state = Box::new(MpvUpdateState {
        needs_update: AtomicBool::new(false),
        ping,
    });
    let update_state_ptr = Box::into_raw(update_state);

    unsafe {
        mpv_render_context_set_update_callback(
            render_ctx,
            mpv_update_callback,
            update_state_ptr as *mut c_void,
        );
    }

    app.mpv_render_ctx = render_ctx;
    app.mpv_update_state = Some(update_state_ptr);
    app.mpv = Some(mpv);

    // ------------------------------------------------------------------
    // Load video into mpv and wait for playback to start
    // ------------------------------------------------------------------

    app.mpv
        .as_mut()
        .unwrap()
        .command("loadfile", &[video_path_str.as_str(), "replace"])
        .map_err(|e| anyhow::anyhow!("Error loading video in mpv: {}", e))?;

    info!("Video loaded, waiting for playback...");

    // Wait for mpv to load the file before checking hwdec.
    let mut hwdec_checked = false;
    {
        let mpv_ref = app.mpv.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match mpv_ref.event_context_mut().wait_event(0.5) {
                Some(Ok(libmpv2::events::Event::FileLoaded)) => {
                    info!("Video loaded by mpv, checking hardware acceleration...");
                    hwdec_checked = true;
                    break;
                }
                Some(Ok(libmpv2::events::Event::EndFile(reason))) => {
                    warn!("mpv: EndFile before loading: {:?}", reason);
                    break;
                }
                Some(Ok(libmpv2::events::Event::Shutdown)) => {
                    anyhow::bail!("mpv se cerró inesperadamente durante carga");
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    warn!("Error in mpv event during loading: {}", fmt_mpv_error(&e));
                    break;
                }
                None => {}
            }
        }
    }

    // Check hardware acceleration (now after FileLoaded).
    if hwdec_checked {
        unsafe {
            let prop = b"hwdec-current\0";
            let mut msg = std::mem::zeroed::<mpv_node>();
            let ret = mpv_get_property(
                app.mpv.as_ref().unwrap().ctx.as_ptr() as *mut crate::bindings::mpv::mpv_handle,
                prop.as_ptr() as *const c_char,
                MPV_FORMAT_STRING,
                &mut msg as *mut _ as *mut c_void,
            );
            if ret >= 0 {
                if !msg.udata.string.is_null() {
                    let hw = std::ffi::CStr::from_ptr(msg.udata.string).to_string_lossy();
                    info!("Hardware acceleration active: {}", hw);
                    libc::free(msg.udata.string as *mut c_void);
                } else {
                    warn!("hwdec-current: (null) — CPU decoding. Consider installing VAAPI for hardware acceleration");
                }
            } else {
                warn!(
                    "Could not query hwdec-current (code {}). CPU decoding.",
                    ret
                );
            }
        }
    }

    info!("Starting render loop...");

    Ok(BootstrapOutput {
        app,
        conn,
        queue,
        ping_source,
    })
}
