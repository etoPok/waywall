//! mpv-wallpaper
//!
//! Renderiza un video como fondo de pantalla animado en Wayland/Hyprland.
//!
//! Arquitectura correcta (Wayland-native, sin hacks X11):
//!   1. Crea una wl_surface + zwlr_layer_surface_v1 (layer: BACKGROUND, fullscreen).
//!   2. Crea un wl_egl_window sobre la surface.
//!   3. Inicializa EGL: EGLDisplay → EGLContext → EGLSurface.
//!   4. Inicializa mpv_render_context (MPV_RENDER_API_TYPE_OPENGL) — mpv NO abre ninguna ventana.
//!   5. Loop: procesa eventos Wayland + mpv, renderiza frames con mpv_render_context_render,
//!      presenta con eglSwapBuffers.
//!
//! Uso:
//!   mpv-wallpaper /ruta/al/video.mp4

use std::{
    env,
    os::raw::{c_char, c_int, c_void},
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use calloop::{ping, timer::Timer, EventLoop, LoopSignal};
use calloop_wayland_source::WaylandSource;
use libc;
use tracing::{debug, error, info, warn};
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_output::{self, WlOutput},
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use libmpv2::{events::Event, Mpv};

// ---------------------------------------------------------------------------
// Bindings EGL (linkea contra libEGL)
// ---------------------------------------------------------------------------

#[link(name = "EGL")]
extern "C" {
    fn eglGetDisplay(native_display: *mut c_void) -> *mut c_void;
    fn eglInitialize(display: *mut c_void, major: *mut c_int, minor: *mut c_int) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        display: *mut c_void,
        attrib_list: *const c_int,
        configs: *mut *mut c_void,
        config_size: c_int,
        num_config: *mut c_int,
    ) -> u32;
    fn eglCreateWindowSurface(
        display: *mut c_void,
        config: *mut c_void,
        native_window: *mut c_void,
        attrib_list: *const c_int,
    ) -> *mut c_void;
    fn eglCreateContext(
        display: *mut c_void,
        config: *mut c_void,
        share_context: *mut c_void,
        attrib_list: *const c_int,
    ) -> *mut c_void;
    fn eglMakeCurrent(
        display: *mut c_void,
        draw: *mut c_void,
        read: *mut c_void,
        ctx: *mut c_void,
    ) -> u32;
    fn eglSwapBuffers(display: *mut c_void, surface: *mut c_void) -> u32;
    fn eglSwapInterval(display: *mut c_void, interval: c_int) -> u32;
    fn eglGetProcAddress(procname: *const c_char) -> *mut c_void;
    fn eglDestroyContext(display: *mut c_void, ctx: *mut c_void) -> u32;
    fn eglDestroySurface(display: *mut c_void, surface: *mut c_void) -> u32;
    fn eglTerminate(display: *mut c_void) -> u32;
}

// ---------------------------------------------------------------------------
// Bindings wayland-egl (wl_egl_window)
// ---------------------------------------------------------------------------

#[link(name = "wayland-egl")]
extern "C" {
    fn wl_egl_window_create(surface: *mut c_void, width: c_int, height: c_int) -> *mut c_void;
    fn wl_egl_window_destroy(egl_window: *mut c_void);
}

// ---------------------------------------------------------------------------
// Bindings mpv render API (libmpv)
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
type mpv_handle = c_void;
#[allow(non_camel_case_types)]
type mpv_render_context = c_void;

const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
const MPV_RENDER_UPDATE_FRAME: u64 = 1;
const MPV_RENDER_PARAM_INVALID: c_int = 0;

#[repr(C)]
struct MpvOpenGLInitParams {
    get_proc_address: extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvOpenGLFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

#[link(name = "mpv")]
extern "C" {
    fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_report_swap(ctx: *mut mpv_render_context);
    fn mpv_render_context_free(ctx: *mut mpv_render_context);
    fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: extern "C" fn(*mut c_void),
        callback_ctx: *mut c_void,
    );
    fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;
    fn mpv_get_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_error_string(error: c_int) -> *const c_char;
}

const MPV_FORMAT_STRING: c_int = 14;

#[repr(C)]
union MpvNodeData {
    string: *mut c_char,
}

#[repr(C)]
struct mpv_node {
    udata: MpvNodeData,
    format: c_int,
}

// ---------------------------------------------------------------------------
// Constantes EGL
// ---------------------------------------------------------------------------

const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_NONE: c_int = 0x3038;
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_WINDOW_BIT: c_int = 0x0004;
const EGL_RENDERABLE_TYPE: c_int = 0x3040;
const EGL_OPENGL_BIT: c_int = 0x0008;
const EGL_RED_SIZE: c_int = 0x3024;
const EGL_GREEN_SIZE: c_int = 0x3023;
const EGL_BLUE_SIZE: c_int = 0x3022;
const EGL_ALPHA_SIZE: c_int = 0x3021;
const EGL_DEPTH_SIZE: c_int = 0x3025;
const EGL_CONTEXT_MAJOR_VERSION: c_int = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: c_int = 0x30FB;
const EGL_NO_DISPLAY: *mut c_void = ptr::null_mut();
const EGL_NO_CONTEXT: *mut c_void = ptr::null_mut();
const EGL_NO_SURFACE: *mut c_void = ptr::null_mut();

// ---------------------------------------------------------------------------
// Estado EGL/mpv render
// ---------------------------------------------------------------------------

struct RenderState {
    egl_display: *mut c_void,
    egl_surface: *mut c_void,
    egl_context: *mut c_void,
    egl_window: *mut c_void,
    render_ctx: *mut mpv_render_context,
    width: i32,
    height: i32,
}

// SAFETY: solo se accede desde el hilo principal
unsafe impl Send for RenderState {}
unsafe impl Sync for RenderState {}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            if !self.render_ctx.is_null() {
                mpv_render_context_free(self.render_ctx);
            }
            eglMakeCurrent(
                self.egl_display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                EGL_NO_CONTEXT,
            );
            if self.egl_surface != EGL_NO_SURFACE {
                eglDestroySurface(self.egl_display, self.egl_surface);
            }
            if self.egl_context != EGL_NO_CONTEXT {
                eglDestroyContext(self.egl_display, self.egl_context);
            }
            if self.egl_display != EGL_NO_DISPLAY {
                eglTerminate(self.egl_display);
            }
            if !self.egl_window.is_null() {
                wl_egl_window_destroy(self.egl_window);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Estado global de la aplicación
// ---------------------------------------------------------------------------

struct App {
    compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,

    surface: Option<WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,

    width: u32,
    height: u32,

    output: Option<WlOutput>,
    loop_signal: Option<LoopSignal>,
    configured: bool,

    /// Puntero al wl_surface nativo (para wl_egl_window_create).
    /// Se obtiene a través de wayland_backend::sys.
    wl_surface_ptr: *mut c_void,

    /// Handle de la cola Wayland, necesario para solicitar frame callbacks.
    qh: Option<QueueHandle<App>>,

    /// Estado de renderizado EGL/mpv.
    render_state: Option<RenderState>,

    /// Instancia de mpv.
    mpv: Option<Mpv>,

    /// true cuando mpv tiene un frame nuevo listo para renderizar.
    /// Puntero raw al MpvUpdateState boxeado; se libera en la limpieza.
    mpv_update_state: Option<*mut MpvUpdateState>,

    /// true cuando se ha solicitado un wl_callback de frame y aún no ha disparado.
    frame_pending: bool,

    /// Callback de frame de Wayland. DEBE mantenerse vivo; si se hace drop,
    /// wayland-client envía wl_proxy_destroy y el compositor cancela el callback.
    wl_callback: Option<WlCallback>,

    /// Primer frame ya renderizado (para no renderizar dos veces).
    first_frame_rendered: bool,

    /// Primer intento de render ya hecho (para renderizar el primer frame sin depender de mpv_update_callback).
    first_render_attempted: bool,

    /// Contador de frames renderizados (para stats periódicas).
    frame_count: u64,

    /// Instante del último log de stats.
    last_stats_time: Option<Instant>,
}

impl App {
    fn new(compositor: WlCompositor, layer_shell: ZwlrLayerShellV1) -> Self {
        Self {
            compositor,
            layer_shell,
            surface: None,
            layer_surface: None,
            width: 0,
            height: 0,
            output: None,
            loop_signal: None,
            configured: false,
            wl_surface_ptr: ptr::null_mut(),
            qh: None,
            render_state: None,
            mpv: None,
            mpv_update_state: None,
            frame_pending: false,
            wl_callback: None,
            first_frame_rendered: false,
            first_render_attempted: false,
            frame_count: 0,
            last_stats_time: None,
        }
    }

    fn create_surfaces(&mut self, qh: &QueueHandle<App>) {
        let surface = self.compositor.create_surface(qh, ());
        let output = self.output.as_ref();

        let layer_surface = self.layer_shell.get_layer_surface(
            &surface,
            output,
            zwlr_layer_shell_v1::Layer::Background,
            "mpv-wallpaper".to_string(),
            qh,
            (),
        );

        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_margin(0, 0, 0, 0);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1);

        // Obtener el puntero C nativo al wl_surface*.
        //
        // wayland_backend::ObjectId::as_ptr() devuelve el *mut wl_proxy nativo
        // a través de la API pública y estable del sys backend.
        self.wl_surface_ptr = proxy_to_raw_ptr(&surface);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);

        info!("Layer surface creada, esperando configure del compositor...");
    }
}

// ---------------------------------------------------------------------------
// Extracción del puntero nativo wl_proxy* de un Proxy de wayland-client 0.31
// ---------------------------------------------------------------------------

/// Devuelve el *mut wl_proxy nativo de cualquier Proxy.
///
/// Requiere en Cargo.toml:
///   wayland-backend = { version = "0.3", features = ["client_system"] }
fn proxy_to_raw_ptr<P: Proxy>(proxy: &P) -> *mut c_void {
    // wayland_backend::ObjectId::as_ptr() devuelve el *mut wl_proxy nativo.
    // Esta es la API pública y estable del sys backend.
    proxy.id().as_ptr() as *mut c_void
}

// ---------------------------------------------------------------------------
// Dispatch impls
// ---------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _state: &mut App,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for App {
    fn event(
        state: &mut App,
        _proxy: &WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            if state.width == 0 {
                info!("Output mode detectado: {}x{}", width, height);
                state.width = width as u32;
                state.height = height as u32;
            }
        }
    }
}

delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore ZwlrLayerShellV1);

impl Dispatch<WlCallback, ()> for App {
    fn event(
        state: &mut App,
        _proxy: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        let wl_callback_count = WL_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
        debug!("WlCallback::event llamado {} veces", wl_callback_count + 1);

        state.frame_pending = false;

        // Solicitar el siguiente frame callback ANTES de render, para que
        // eglSwapBuffers (dentro de render_frame) commitee la surface
        // incluyendo esta solicitud. Sin commit, el compositor ignora el
        // wl_callback y el loop de frames muere.
        if let Some(surface) = &state.surface {
            if let Some(qh) = &state.qh {
                state.wl_callback = Some(surface.frame(qh, ()));
                state.frame_pending = true;
            }
        }

        // render_frame SIEMPRE llama eglSwapBuffers (con o sin frame),
        // lo que commitea la surface y entrega el frame request al servidor.
        // Internamente también llama mpv_render_context_update() que rearma
        // mpv_update_callback para el próximo frame.
        if let Some(rs) = &mut state.render_state {
            if unsafe { render_frame(rs) } {
                state.frame_count += 1;
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        state: &mut App,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                // info!("Configure recibido: {}x{}", width, height);
                if width > 0 {
                    state.width = width;
                }
                if height > 0 {
                    state.height = height;
                }
                proxy.ack_configure(serial);
                if let Some(surface) = &state.surface {
                    surface.commit();
                }
                state.configured = true;
                // info!("Surface configurada: {}x{}", state.width, state.height);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                warn!("Layer surface cerrada por el compositor");
                if let Some(signal) = &state.loop_signal {
                    signal.stop();
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// get_proc_address callback para mpv_render_context
// ---------------------------------------------------------------------------

extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { eglGetProcAddress(name) }
}

// ---------------------------------------------------------------------------
// Callback de actualización de mpv — despierta el event loop cuando hay frame
// ---------------------------------------------------------------------------

/// Estado compartido entre el callback de mpv (hilo de decodificación)
/// y el event loop principal.
struct MpvUpdateState {
    needs_update: AtomicBool,
    ping: ping::Ping,
}

extern "C" fn mpv_update_callback(ctx: *mut c_void) {
    let count = UPDATE_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    debug!("mpv_update_callback llamado {} veces", count + 1);

    unsafe {
        let state = &*(ctx as *const MpvUpdateState);
        state.needs_update.store(true, Ordering::SeqCst);
        state.ping.ping();
    }
}

/// Callback vacío para desregistrar el update callback de mpv en la limpieza.
extern "C" fn noop_update_callback(_ctx: *mut c_void) {}

// ---------------------------------------------------------------------------
// Inicialización EGL
// ---------------------------------------------------------------------------

/// Inicializa EGL/OpenGL sobre la wl_surface dada.
///
/// Recibe:
///   - wl_display_ptr: puntero al wl_display* (de Connection::backend().display_ptr())
///   - wl_surface_ptr: puntero al wl_surface* (de proxy_to_raw_ptr)
///   - width, height: dimensiones del output
///
/// Devuelve: (egl_display, egl_surface, egl_context, egl_window)
unsafe fn init_egl(
    wl_display_ptr: *mut c_void,
    wl_surface_ptr: *mut c_void,
    width: i32,
    height: i32,
) -> Result<(*mut c_void, *mut c_void, *mut c_void, *mut c_void)> {
    let egl_display = eglGetDisplay(wl_display_ptr);
    if egl_display == EGL_NO_DISPLAY {
        bail!("eglGetDisplay falló");
    }

    let mut major: c_int = 0;
    let mut minor: c_int = 0;
    if eglInitialize(egl_display, &mut major, &mut minor) == 0 {
        bail!("eglInitialize falló");
    }
    info!("EGL {}.{} inicializado", major, minor);

    if eglBindAPI(EGL_OPENGL_API) == 0 {
        bail!("eglBindAPI(OPENGL) falló");
    }

    #[rustfmt::skip]
    let attribs_config: [c_int; 15] = [
        EGL_SURFACE_TYPE,    EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
        EGL_RED_SIZE,        8,
        EGL_GREEN_SIZE,      8,
        EGL_BLUE_SIZE,       8,
        EGL_ALPHA_SIZE,      8,
        EGL_DEPTH_SIZE,      0,
        EGL_NONE,
    ];
    let mut egl_config: *mut c_void = ptr::null_mut();
    let mut num_configs: c_int = 0;
    if eglChooseConfig(
        egl_display,
        attribs_config.as_ptr(),
        &mut egl_config,
        1,
        &mut num_configs,
    ) == 0
        || num_configs == 0
    {
        bail!("eglChooseConfig falló o no encontró configuraciones válidas");
    }

    let egl_window = wl_egl_window_create(wl_surface_ptr, width, height);
    if egl_window.is_null() {
        bail!("wl_egl_window_create falló");
    }

    let egl_surface = eglCreateWindowSurface(egl_display, egl_config, egl_window, ptr::null());
    if egl_surface == EGL_NO_SURFACE {
        wl_egl_window_destroy(egl_window);
        bail!("eglCreateWindowSurface falló");
    }

    #[rustfmt::skip]
    let attribs_ctx: [c_int; 5] = [
        EGL_CONTEXT_MAJOR_VERSION, 3,
        EGL_CONTEXT_MINOR_VERSION, 3,
        EGL_NONE,
    ];
    let egl_context = eglCreateContext(
        egl_display,
        egl_config,
        EGL_NO_CONTEXT,
        attribs_ctx.as_ptr(),
    );
    if egl_context == EGL_NO_CONTEXT {
        eglDestroySurface(egl_display, egl_surface);
        wl_egl_window_destroy(egl_window);
        bail!("eglCreateContext falló (requiere OpenGL >= 3.3)");
    }

    if eglMakeCurrent(egl_display, egl_surface, egl_surface, egl_context) == 0 {
        eglDestroyContext(egl_display, egl_context);
        eglDestroySurface(egl_display, egl_surface);
        wl_egl_window_destroy(egl_window);
        bail!("eglMakeCurrent falló");
    }

    // Sin vsync forzado a nivel EGL; mpv gestiona su propio timing
    eglSwapInterval(egl_display, 0);

    info!("EGL inicializado correctamente ({}x{})", width, height);
    Ok((egl_display, egl_surface, egl_context, egl_window))
}

// ---------------------------------------------------------------------------
// Inicialización mpv — SIN vo, SIN wid, SIN gpu-context
// ---------------------------------------------------------------------------

fn init_mpv(gpu_api: &str) -> Result<Mpv> {
    let mpv = Mpv::with_initializer(|init| {
        init.set_property("terminal", "no")?;
        init.set_property("msg-level", "all=warn,vd=info")?;
        init.set_property("loop-file", "inf")?;
        init.set_property("loop", "inf")?;
        init.set_property("mute", true)?;
        init.set_property("audio", false)?;
        init.set_property("osc", false)?;
        init.set_property("osd-level", 0_i64)?;
        init.set_property("pause", false)?;
        init.set_property("hwdec", "auto-safe")?;
        init.set_property("keepaspect", false)?;
        init.set_property("input-default-bindings", false)?;
        init.set_property("input-vo-keyboard", false)?;
        init.set_property("input-cursor", false)?;
        init.set_property("vo", "libmpv")?;
        // Render API (vo=libmpv) solo soporta OpenGL. Ignorar el --gpu-api del usuario.
        init.set_property("gpu-api", "opengl")?;
        // NOTA: gpu-context NO se setea aquí; con vo=libmpv el contexto lo provee
        // la aplicación via mpv_render_context_create.
        // display-resample requiere timing del compositor que vo=libmpv no provee
        init.set_property("video-sync", "audio")?;
        // framedrop=vo es UB con vo=libmpv
        init.set_property("framedrop", "no")?;
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("Error inicializando libmpv: {}", e))?;

    info!(
        "libmpv inicializado (gpu-api={}, modo render API, sin ventana propia)",
        gpu_api
    );
    Ok(mpv)
}

// ---------------------------------------------------------------------------
// Crear mpv_render_context sobre el EGLContext activo
// ---------------------------------------------------------------------------

unsafe fn create_render_context(mpv: &Mpv) -> Result<*mut mpv_render_context> {
    // libmpv2 expone el handle nativo mediante el campo `ctx` (NonNull<mpv_handle>)
    let mpv_handle_ptr = mpv.ctx.as_ptr() as *mut mpv_handle;

    let mut opengl_init_params = MpvOpenGLInitParams {
        get_proc_address,
        get_proc_address_ctx: ptr::null_mut(),
    };

    let api_type_ptr = MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *const c_char;

    let mut params = [
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_API_TYPE,
            data: api_type_ptr as *mut c_void,
        },
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: &mut opengl_init_params as *mut _ as *mut c_void,
        },
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];

    let mut render_ctx: *mut mpv_render_context = ptr::null_mut();
    let ret = mpv_render_context_create(&mut render_ctx, mpv_handle_ptr, params.as_mut_ptr());
    if ret < 0 {
        bail!("mpv_render_context_create falló con código {}", ret);
    }

    info!("mpv_render_context creado correctamente");
    Ok(render_ctx)
}

// ---------------------------------------------------------------------------
// Renderizar un frame al FBO 0 (framebuffer de la EGLSurface)
// ---------------------------------------------------------------------------

/// Devuelve true si se renderizó un frame, false si no había frame nuevo.
unsafe fn render_frame(rs: &mut RenderState) -> bool {
    let flags = mpv_render_context_update(rs.render_ctx);
    let has_frame = flags & MPV_RENDER_UPDATE_FRAME != 0;

    if has_frame {
        let mut fbo = MpvOpenGLFbo {
            fbo: 0,
            w: rs.width,
            h: rs.height,
            internal_format: 0,
        };
        let mut flip_y: c_int = 1;

        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip_y as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        mpv_render_context_render(rs.render_ctx, params.as_mut_ptr());
        eglSwapBuffers(rs.egl_display, rs.egl_surface);
        mpv_render_context_report_swap(rs.render_ctx);
    } else {
        // Swap sin render para commitar la surface Wayland.
        // Sin commit el wl_callback.frame() nunca se procesa y el
        // loop de frame callbacks muere.
        eglSwapBuffers(rs.egl_display, rs.egl_surface);
    }
    has_frame
}

// ---------------------------------------------------------------------------
// Procesar eventos mpv
// ---------------------------------------------------------------------------

/// Convierte un error de mpv a su descripción textual usando mpv_error_string.
fn fmt_mpv_error(e: &libmpv2::Error) -> String {
    match e {
        libmpv2::Error::Raw(code) => {
            let s = unsafe {
                let ptr = mpv_error_string(*code);
                if ptr.is_null() {
                    format!("Raw({}) (unknown)", code)
                } else {
                    let cstr = std::ffi::CStr::from_ptr(ptr);
                    format!("Raw({}): {}", code, cstr.to_string_lossy())
                }
            };
            s
        }
        _ => format!("{}", e),
    }
}

fn process_mpv_events(mpv: &mut Mpv, loop_signal: &Option<LoopSignal>) {
    loop {
        match mpv.event_context_mut().wait_event(0.0) {
            Some(Ok(Event::EndFile(reason))) => {
                warn!("mpv: EndFile ({:?}), el loop debería reiniciar", reason);
            }
            Some(Ok(Event::Shutdown)) => {
                error!("mpv se cerró inesperadamente");
                if let Some(signal) = loop_signal {
                    signal.stop();
                }
                break;
            }
            Some(Ok(Event::LogMessage { text, .. })) => {
                tracing::debug!("mpv: {}", text.trim());
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                error!("Error en evento mpv: {}", fmt_mpv_error(&e));
                break;
            }
            None => break,
        }
    }
}


// ---------------------------------------------------------------------------
// Setup Ctrl+C / SIGTERM
// ---------------------------------------------------------------------------

static TERMINATE: AtomicBool = AtomicBool::new(false);

static UPDATE_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static WL_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

fn ctrlc_setup(loop_signal: LoopSignal) {
    unsafe {
        extern "C" fn handle_signal(_sig: libc::c_int) {
            TERMINATE.store(true, Ordering::Relaxed);
        }
        type SigFn = unsafe extern "C" fn(libc::c_int);
        let handler = handle_signal as SigFn as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        if TERMINATE.load(Ordering::Relaxed) {
            info!("Señal recibida, cerrando...");
            loop_signal.stop();
            break;
        }
    });
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mpv_wallpaper=info".parse().unwrap()),
        )
        .init();

    let args: Vec<String> = env::args().collect();

    let mut gpu_api = String::from("auto");
    let mut video_path: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--gpu-api" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --gpu-api requiere un valor (opengl, vulkan, auto)");
                    std::process::exit(1);
                }
                gpu_api = args[i].clone();
                if !["opengl", "vulkan", "auto"].contains(&gpu_api.as_str()) {
                    eprintln!("Error: --gpu-api debe ser 'opengl', 'vulkan' o 'auto'");
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                eprintln!("Uso: {} [OPCIONES] <ruta-al-video>", args[0]);
                eprintln!();
                eprintln!("Opciones:");
                eprintln!("  --gpu-api <opengl|vulkan|auto>  API de rendering GPU (default: auto)");
                eprintln!("  -h, --help                     Mostrar esta ayuda");
                eprintln!();
                eprintln!(
                    "Ejemplo: {} --gpu-api vulkan /home/user/wallpaper.mp4",
                    args[0]
                );
                std::process::exit(0);
            }
            _ => {
                if video_path.is_none() {
                    video_path = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let video_path = match video_path {
        Some(p) => p,
        None => {
            eprintln!("Uso: {} [OPCIONES] <ruta-al-video>", args[0]);
            eprintln!("Ejemplo: {} /home/user/wallpaper.mp4", args[0]);
            std::process::exit(1);
        }
    };

    info!(
        "mpv-wallpaper iniciando con video: {} (gpu-api: {})",
        video_path, gpu_api
    );

    // Validar que el archivo de video existe antes de continuar
    let video_path = std::path::Path::new(&video_path);
    if !video_path.exists() {
        bail!(
            "El archivo de video no existe: {}",
            video_path.display()
        );
    }
    let video_path = video_path
        .canonicalize()
        .context("Error resolviendo la ruta del video")?;
    let video_path_str = video_path.to_string_lossy().to_string();

    // ------------------------------------------------------------------
    // 1. Conectar a Wayland
    // ------------------------------------------------------------------

    let conn = Connection::connect_to_env()
        .context("No se pudo conectar al servidor Wayland. ¿Está WAYLAND_DISPLAY seteado?")?;

    // Obtener el puntero C al wl_display* para EGL.
    // En wayland-client 0.31, Connection::backend() devuelve Backend directamente.
    // Backend::display_ptr() devuelve el *mut wl_display nativo.
    let wl_display_ptr = { conn.backend().display_ptr() as *mut c_void };

    let (globals, mut queue) =
        registry_queue_init::<App>(&conn).context("Error inicializando registry Wayland")?;
    let qh = queue.handle();

    let compositor: WlCompositor = globals
        .bind(&qh, 4..=5, ())
        .context("El compositor no soporta wl_compositor")?;

    let layer_shell: ZwlrLayerShellV1 = globals
        .bind(&qh, 1..=4, ())
        .context("El compositor no soporta zwlr_layer_shell_v1. ¿Está usando Hyprland?")?;

    let output: Option<WlOutput> = globals.bind(&qh, 1..=4, ()).ok();
    if output.is_none() {
        warn!("No se detectó wl_output, el compositor asignará el monitor");
    }

    // ------------------------------------------------------------------
    // 2. Estado inicial
    // ------------------------------------------------------------------

    let mut app = App::new(compositor, layer_shell);
    app.output = output;
    app.qh = Some(qh.clone());

    queue
        .roundtrip(&mut app)
        .context("Error en roundtrip inicial")?;

    if app.width == 0 || app.height == 0 {
        warn!("Dimensiones del output no detectadas, usando 1920x1080 como fallback");
        app.width = 1920;
        app.height = 1080;
    }

    // ------------------------------------------------------------------
    // 3. Crear layer-shell surface
    // ------------------------------------------------------------------

    app.create_surfaces(&qh);

    let mut configure_attempts = 0;
    while !app.configured && configure_attempts < 50 {
        queue
            .blocking_dispatch(&mut app)
            .context("Error esperando configure")?;
        configure_attempts += 1;
    }

    if !app.configured {
        bail!(
            "El compositor no envió configure tras {} intentos.",
            configure_attempts
        );
    }

    let wl_surface_ptr = app.wl_surface_ptr;
    if wl_surface_ptr.is_null() {
        bail!("No se pudo obtener el puntero nativo de la wl_surface");
    }

    let width = app.width as i32;
    let height = app.height as i32;

    // ------------------------------------------------------------------
    // 4. Inicializar EGL/OpenGL
    // ------------------------------------------------------------------

    let (egl_display, egl_surface, egl_context, egl_window) = unsafe {
        init_egl(wl_display_ptr, wl_surface_ptr, width, height)
            .context("Error inicializando EGL")?
    };

    // ------------------------------------------------------------------
    // 5. Inicializar libmpv
    // ------------------------------------------------------------------

    let mpv = init_mpv(&gpu_api)?;

    // ------------------------------------------------------------------
    // 6. Crear mpv_render_context sobre el EGLContext activo
    // ------------------------------------------------------------------

    let render_ctx =
        unsafe { create_render_context(&mpv).context("Error creando mpv_render_context")? };

    let rs = RenderState {
        egl_display,
        egl_surface,
        egl_context,
        egl_window,
        render_ctx,
        width,
        height,
    };

    // ------------------------------------------------------------------
    // 7. Configurar update callback de mpv + mecanismo de wakeup
    // ------------------------------------------------------------------

    let (ping, ping_source) = ping::make_ping().context("Error creando ping para wakeup")?;

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

    app.mpv_update_state = Some(update_state_ptr);

    // Guardar estado de renderizado y mpv en App.
    app.render_state = Some(rs);
    app.mpv = Some(mpv);

    // ------------------------------------------------------------------
    // 8. Cargar video en mpv y esperar a que empiece la reproducción
    // ------------------------------------------------------------------

    app.mpv
        .as_mut()
        .unwrap()
        .command("loadfile", &[video_path_str.as_str(), "replace"])
        .map_err(|e| anyhow::anyhow!("Error cargando video en mpv: {}", e))?;

    info!("Video cargado, esperando reproducción...");

    // Esperar a que mpv cargue el archivo antes de verificar hwdec.
    let mut hwdec_checked = false;
    {
        let mpv_ref = app.mpv.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match mpv_ref.event_context_mut().wait_event(0.5) {
                Some(Ok(Event::FileLoaded)) => {
                    info!("Video cargado por mpv, verificando aceleración por hardware...");
                    hwdec_checked = true;
                    break;
                }
                Some(Ok(Event::EndFile(reason))) => {
                    warn!("mpv: EndFile antes de cargar: {:?}", reason);
                    break;
                }
                Some(Ok(Event::Shutdown)) => {
                    bail!("mpv se cerró inesperadamente durante carga");
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    warn!("Error en evento mpv durante carga: {}", fmt_mpv_error(&e));
                    break;
                }
                None => {}
            }
        }
    }

    // Verificar aceleración por hardware (ahora sí, después de FileLoaded).
    if hwdec_checked {
        unsafe {
            let prop = b"hwdec-current\0";
            let mut msg = std::mem::zeroed::<mpv_node>();
            let ret = mpv_get_property(
                app.mpv.as_ref().unwrap().ctx.as_ptr() as *mut mpv_handle,
                prop.as_ptr() as *const c_char,
                MPV_FORMAT_STRING,
                &mut msg as *mut _ as *mut c_void,
            );
            if ret >= 0 {
                if !msg.udata.string.is_null() {
                    let hw = std::ffi::CStr::from_ptr(msg.udata.string).to_string_lossy();
                    info!("Aceleración por hardware activa: {}", hw);
                    libc::free(msg.udata.string as *mut c_void);
                } else {
                    warn!("hwdec-current: (null) — decodificación por CPU. Considera instalar VAAPI o usar --gpu-api vulkan");
                }
            } else {
                warn!(
                    "No se pudo consultar hwdec-current (código {}). Decodificación por CPU.",
                    ret
                );
            }
        }
    }

    info!("Iniciando render loop...");

    // ------------------------------------------------------------------
    // 9. Event loop principal — sin polling, solo wakeup por mpv o Wayland
    // ------------------------------------------------------------------

    let mut event_loop: EventLoop<App> =
        EventLoop::try_new().context("Error creando event loop")?;

    let loop_signal = event_loop.get_signal();
    app.loop_signal = Some(loop_signal.clone());

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registrando fuente Wayland en event loop: {}", e))?;

    // Insertar PingSource: despierta el event loop cuando mpv llama al update callback.
    event_loop
        .handle()
        .insert_source(ping_source, |(), &mut (), _| {})
        .map_err(|e| anyhow::anyhow!("Error registrando PingSource en event loop: {}", e))?;

    // Timer para stats periódicas de rendimiento (cada 5 segundos).
    let stats_timer = Timer::from_duration(Duration::from_secs(5));
    event_loop
        .handle()
        .insert_source(stats_timer, |_, _, app| {
            if let Some(mpv) = &app.mpv {
                let frames = app.frame_count;
                let elapsed = app
                    .last_stats_time
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(5.0);
                let fps = if elapsed > 0.0 {
                    frames as f64 / elapsed
                } else {
                    0.0
                };

                // Consultar frame-drop del decoder.
                if let Ok(val) = mpv.get_property::<i64>("decoder-frame-drop-count") {
                    if val > 0 {
                        warn!(
                            "Stats: {:.1} fps, {} frames, decoder-drops: {}",
                            fps, frames, val
                        );
                    } else {
                        info!("Stats: {:.1} fps, {} frames, sin drops", fps, frames);
                    }
                } else {
                    info!("Stats: {:.1} fps, {} frames", fps, frames);
                }

                // Consultar fps estimado del video.
                if let Ok(val) = mpv.get_property::<f64>("estimated-vf-fps") {
                    info!("  estimated-vf-fps: {:.2}", val);
                }
            }
            app.frame_count = 0;
            app.last_stats_time = Some(Instant::now());
            // Re-programar el timer para las próximas stats.
            calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(5))
        })
        .map_err(|e| anyhow::anyhow!("Error registrando stats timer: {}", e))?;

    app.last_stats_time = Some(Instant::now());

    info!("Event loop iniciado (sin polling). Ctrl+C para salir.");
    ctrlc_setup(loop_signal);

    // Se duerme indefinidamente hasta que mpv o Wayland despierten el loop.
    // No hay temporizador periódico — consumo de CPU ≈ 0 cuando no hay frames.
    event_loop
        .run(None, &mut app, |app| {
            if let Some(mpv) = &mut app.mpv {
                process_mpv_events(mpv, &app.loop_signal);
            }

            // Primer frame: solicitar frame callback ANTES de render para que
            // eglSwapBuffers commitee la surface incluyendo el frame request.
            if !app.first_render_attempted {
                app.first_render_attempted = true;
                if let Some(surface) = &app.surface {
                    if let Some(qh) = &app.qh {
                        app.wl_callback = Some(surface.frame(qh, ()));
                        app.frame_pending = true;
                    }
                }
                if let Some(rs) = &mut app.render_state {
                    if unsafe { render_frame(rs) } {
                        app.frame_count += 1;
                    }
                    app.first_frame_rendered = true;
                }
            }

            // Cuando mpv tiene datos nuevos (mpv_update_callback), solicitar
            // un frame de Wayland. El render REAL ocurre en Dispatch<WlCallback>
            // (vsync), donde render_frame llama mpv_render_context_update que
            // rearma el callback para el siguiente frame.
            let needs_render = app
                .mpv_update_state
                .map(|ptr| unsafe { (*ptr).needs_update.swap(false, Ordering::SeqCst) })
                .unwrap_or(false);

            if needs_render && !app.frame_pending {
                if let Some(surface) = &app.surface {
                    if let Some(qh) = &app.qh {
                        app.wl_callback = Some(surface.frame(qh, ()));
                        app.frame_pending = true;
                    }
                }
            }
        })
        .context("Error en event loop")?;

    // ------------------------------------------------------------------
    // 10. Limpieza
    // ------------------------------------------------------------------

    info!("Saliendo limpiamente...");

    unsafe {
        mpv_render_context_set_update_callback(render_ctx, noop_update_callback, ptr::null_mut());
    }

    // Liberar el MpvUpdateState boxeado.
    if let Some(state_ptr) = app.mpv_update_state.take() {
        unsafe { drop(Box::from_raw(state_ptr)) };
    }

    if let Some(rs) = app.render_state.take() {
        drop(rs);
    }
    if let Some(mpv) = app.mpv.take() {
        drop(mpv);
    }

    if let Some(ls) = app.layer_surface.take() {
        ls.destroy();
    }
    if let Some(s) = app.surface.take() {
        s.destroy();
    }

    info!("Salida completa.");
    Ok(())
}
