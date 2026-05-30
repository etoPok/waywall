# mpv-wallpaper - Contexto del Proyecto

## Descripción

Aplicación en Rust que renderiza un video como fondo de pantalla animado en Wayland/Hyprland. Usa la **mpv render API** (`vo=libmpv`) + EGL/OpenGL + Wayland layer-shell para dibujar directamente sobre la capa BACKGROUND del compositor, sin abrir ventanas adicionales.

**Nota:** El README anterior describía incorrectamente un enfoque `wid`. El código real usa `mpv_render_context` con FBO.

## Arquitectura

```
Video → libmpv (vo=libmpv, render API) → mpv_render_context_render → EGL FBO 0 → eglSwapBuffers → wl_surface → Hyprland BACKGROUND layer
```

Flujo paso a paso:
1. Conectar a Wayland, bind `WlCompositor` + `ZwlrLayerShellV1` + `WlOutput`
2. Crear `wl_surface` + `zwlr_layer_surface_v1` (layer BACKGROUND, anchor todos los bordes, exclusive_zone=-1)
3. Obtener puntero nativo `wl_surface*` vía `wayland-backend` (`ObjectId::as_ptr()`)
4. Inicializar EGL: `eglGetDisplay(wl_display)` → `eglInitialize` → `eglBindAPI(OPENGL)` → `eglChooseConfig` → `wl_egl_window_create(surface)` → `eglCreateWindowSurface` → `eglCreateContext(OpenGL 3.3)` → `eglMakeCurrent` → `eglSwapInterval(0)`
5. Inicializar mpv con `vo=libmpv`, `gpu-api=opengl` (sin `gpu-context`, el contexto lo provee la app)
6. `mpv_render_context_create` con el EGLContext activo + callback `get_proc_address`
7. Registrar `mpv_render_context_set_update_callback` → `MpvUpdateState` (AtomicBool + calloop::Ping)
8. `loadfile` → esperar `Event::FileLoaded`
9. Event loop (calloop): wake por Wayland events o mpv ping
10. Render: `mpv_render_context_update()` → si `MPV_RENDER_UPDATE_FRAME` → `mpv_render_context_render(fbo=0)` → `eglSwapBuffers` → `mpv_render_context_report_swap`

## Stack Técnico

- **Lenguaje:** Rust (edition 2021)
- **Compilación:** `cargo build --release`
- **Link flags:** `-lEGL -lwayland-egl -lGL -lmpv` (en `.cargo/config.toml`)
- **Dependencias:**
  - `wayland-client 0.31` — protocolo Wayland client
  - `wayland-protocols 0.31` (features: client, unstable) — extensiones Wayland
  - `wayland-protocols-wlr 0.2` (features: client) — wlr-layer-shell
  - `wayland-backend 0.3` (features: client_system) — acceso nativo wl_proxy
  - `smithay-client-toolkit 0.18` (features: calloop, xkbcommon) — abstracciones Wayland (mínimo uso)
  - `calloop 0.13` — event loop
  - `calloop-wayland-source 0.3` — integración Wayland para calloop
  - `libmpv2 3` — bindings Rust para libmpv
  - `anyhow 1` — manejo de errores
  - `tracing 0.1` + `tracing-subscriber 0.3` (env-filter) — logging
  - `libc 0.2` — señales POSIX

## Estructura de Archivos

```
mpv-wallpaper/
├── Cargo.toml
├── .cargo/config.toml          # Link flags EGL/GL/mpv
├── src/
│   └── main.rs                 # Código completo (1268 líneas, archivo único)
├── README.md
├── CONTEXT.md                  # Este archivo
├── BUGS.txt                    # Bugs conocidos
├── plan_optimizacion_1.txt     # Roadmap de optimizaciones
└── log.txt                     # Logs de ejecución
```

## Uso

```bash
# Básico
cargo run --release -- /ruta/al/video.mp4

# Con GPU API (notas: --gpu-api es aceptado pero siempre se fuerza opengl internamente)
cargo run --release -- --gpu-api vulkan /ruta/al/video.mp4

# Con logging verbose
RUST_LOG=mpv_wallpaper=debug cargo run --release -- /ruta/al/video.mp4
```

CLI flags:
- `--gpu-api <opengl|vulkan|auto>` (default: auto) — **aceptado pero ignorado**, siempre se fuerza `opengl` en `init_mpv()` línea 655
- `-h, --help`

## Estado Actual y Bug Crítico

**El video no se renderiza** — solo muestra fondo oscuro (0 fps, 0 frames).

Evidencia de `log.txt`:
1. mpv carga el archivo (`Event::FileLoaded` ok)
2. `mpv_update_callback` se invoca 19 veces (debug) o nunca llegan `WlCallback::event`
3. `hwdec-current` falla con código -9 (propiedad no disponible en ese momento)
4. Stats muestran `0.0 fps, 0 frames` — mpv decodifica internamente (`estimated-vf-fps: 30.00`) pero nunca se renderiza

**Causa raíz:**
`mpv_render_context_update()` (línea 719) **nunca retorna `MPV_RENDER_UPDATE_FRAME`** — el flag se queda en 0. Como `render_frame()` siempre retorna `false`, nunca hay un render efectivo. Sin `eglSwapBuffers` con contenido nuevo, Wayland nunca recibe el frame callback request, y `WlCallback::event` nunca se dispara. Resultado: deadlock.

**Chain de causas:**
1. `mpv_render_context_update()` no retorna `MPV_RENDER_UPDATE_FRAME`
2. `render_frame()` retorna `false` → no se llama `mpv_render_context_render`
3. `eglSwapBuffers` solo hace commit sin contenido nuevo
4. `wl_surface.frame()` request nunca llega al compositor
5. `WlCallback::event` nunca se dispara
6. El render loop nunca arranca

## Funciones y Líneas Clave

### FFI Declarations (53-198)
| Tipo | Líneas | Descripción |
|------|--------|-------------|
| EGL extern "C" | 53-89 | eglGetDisplay, eglInitialize, eglCreateContext, etc. |
| wayland-egl extern "C" | 95-99 | wl_egl_window_create, wl_egl_window_destroy |
| mpv render API types | 105-177 | mpv_handle, mpv_render_context, constants, structs |
| EGL constants | 183-198 | EGL_OPENGL_API, EGL_NONE, etc. |

### Structs (204-299)
| Struct | Líneas | Descripción |
|--------|--------|-------------|
| `RenderState` | 204-212 | Estado EGL + render_ctx + dimensiones |
| `RenderState` Drop | 218-244 | Libera render context, EGL, egl_window |
| `App` | 250-299 | Estado global (Wayland + mpv + render + frame tracking) |
| `MpvUpdateState` | 512-515 | AtomicBool needs_update + calloop::Ping |

### Funciones principales
| Función | Líneas | Descripción |
|---------|--------|-------------|
| `App::new()` | 302-325 | Constructor |
| `App::create_surfaces()` | 327-364 | Crea wl_surface + layer_surface (BACKGROUND) |
| `proxy_to_raw_ptr()` | 375-379 | Extrae `wl_proxy*` nativo de un Proxy |
| `get_proc_address()` | 502-504 | Callback EGL proc address para mpv |
| `mpv_update_callback()` | 517-526 | Set needs_update + ping event loop |
| `noop_update_callback()` | 529 | Callback vacío para cleanup |
| `init_egl()` | 543-631 | Inicialización completa EGL/OpenGL 3.3 |
| `init_mpv()` | 637-671 | Configuración mpv (loop, mute, vo=libmpv, gpu-api=opengl) |
| `create_render_context()` | 677-711 | mpv_render_context_create |
| `render_frame()` | 718-756 | Render: mpv_render_context_update → if frame: render + swap + report_swap |
| `fmt_mpv_error()` | 763-779 | Convierte errores mpv a strings |
| `process_mpv_events()` | 781-805 | Drena cola de eventos mpv |
| `ctrlc_setup()` | 817-835 | Handler SIGINT/SIGTERM |
| `main()` | 841-1268 | Entry point completo |

### Dispatch Implementations
| Impl | Líneas | Descripción |
|------|--------|-------------|
| `Dispatch<WlRegistry, GlobalListContents>` | 385-395 | No-op |
| `Dispatch<WlOutput, ()>` | 397-414 | Captura dimensiones del output |
| `Dispatch<WlCallback, ()>` | 421-456 | **Render principal**: reset frame_pending → request next frame → render_frame |
| `Dispatch<ZwlrLayerSurfaceV1, ()>` | 458-496 | Handle Configure (ack + commit) y Closed |
| `delegate_noop!` | 416-419 | WlSurface, WlCompositor, WlSeat, ZwlrLayerShellV1 ignorados |

### Event Loop (dentro de main)
| Componente | Líneas | Descripción |
|-----------|--------|-------------|
| WaylandSource | 1133-1135 | Registra fuente Wayland |
| PingSource | 1138-1141 | Wake por mpv_update_callback |
| Stats timer | 1144-1184 | Cada 5s logea fps, frame_count, decoder-drops, estimated-vf-fps |
| Idle callback | 1194-1233 | Primer frame + check needs_render → request wl_callback |
| Cleanup | 1237-1267 | Deregistra update callback, libera MpvUpdateState, drop en orden |

## Optimizaciones Implementadas

1. **Zero-polling:** `event_loop.run(None, ...)` — sin timer periódico
2. **mpv update callback:** `mpv_render_context_set_update_callback` + `calloop::ping`
3. **Frame callback Wayland:** `wl_surface.frame()` para sync con compositor
4. **EGL vsync off:** `eglSwapInterval(display, 0)` — mpv gestiona timing
5. **Stats periódicas:** Timer calloop cada 5s logea fps/drops
6. **hwdec check post-FileLoaded:** Espera `Event::FileLoaded` antes de consultar
7. **CLI `--gpu-api`:** Aceptado pero siempre fuerza opengl (render API solo soporta OpenGL)
8. **Render path único:** Solo `WlCallback::event` ejecuta `render_frame()` (post-fix)
9. **eglSwapBuffers sin frame:** En `render_frame()` cuando no hay frame nuevo, hace swap para commitear surface y mantener vivo el loop de frame callbacks

## Restricciones/Consideraciones

- **Single-monitor only:** Solo un `wl_output` se bindea (línea 942), sin lógica multi-output
- **OpenGL 3.3 requerido:** Puede fallar a software rendering en drivers antiguos
- **`unsafe` extenso:** FFI calls (EGL, wayland-egl, mpv render API), raw pointer derefs, `Box::into_raw`/`Box::from_raw`
- **`render_ctx` es raw pointer:** Se libera en `RenderState::drop`
- **`MpvUpdateState` es Box raw:** Se libera manualmente en cleanup (línea 1248-1250)
- **`proxy_to_raw_ptr()` usa `wayland-backend`:** Con feature `client_system`
- **`--gpu-api` ignorado:** Siempre se fuerza `opengl` porque mpv render API solo soporta OpenGL
- **Resize no implementado:** No se llama `wl_egl_window_resize` en Configure events
- **`smithay-client-toolkit`:** Importado pero barely used (solo features calloop/xkbcommon)
