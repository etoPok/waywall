# mpv-wallpaper

mpv-wallpaper le permite reproducir videos como fondos de pantalla usando mpv. Es una herramienta minima para Hyprland/Wayland.

## Cómo funciona

```
Tu video.mp4
     │
     ▼
  libmpv                          Wayland
  ──────                          ───────
  decodifica H.264/HEVC/AV1  →   zwlr_layer_shell_v1
  hardware decoding (vaapi)   →   layer: BACKGROUND
  vo=libmpv (render API)      →   anchors: top+bottom+left+right
  gpu-api=opengl               →   wl_surface → wl_egl_window
     │
     └──► mpv_render_context_render() → FBO 0 → eglSwapBuffers → surface
```

**mpv hace TODO el trabajo pesado de decodificación y render.** Este programa solo:
1. Abre la conexión Wayland y negocia una `zwlr_layer_surface_v1` en capa BACKGROUND
2. Inicializa EGL/OpenGL y crea un `mpv_render_context` sobre ese contexto
3. Corre un event loop que renderiza frames cuando mpv lo solicita

No hay decodificación de video manual. mpv se encarga de todo eso internamente a través de su render API.

## Dependencias del sistema

### Runtime

```bash
# Arch Linux / Manjaro
sudo pacman -S mpv

# Ubuntu 24.04 / Debian Bookworm
sudo apt install libmpv-dev libmpv2

# Fedora
sudo dnf install mpv-libs
```

### Build

```bash
# Arch Linux
sudo pacman -S mpv pkg-config

# Ubuntu / Debian
sudo apt install libmpv-dev pkg-config build-essential

# Fedora
sudo dnf install mpv-devel pkg-config gcc
```

Verificar que pkg-config encuentra libmpv:

```bash
pkg-config --modversion mpv
# Debe imprimir algo como: 0.37.0
```

### Versiones mínimas recomendadas

| Componente | Mínimo | Recomendado |
|------------|--------|-------------|
| mpv / libmpv | 0.35 | 0.37+ |
| Rust | 1.70 | stable reciente |
| Hyprland | 0.30 | 0.40+ |
| Mesa / drivers gráficos | cualquiera con OpenGL 3.3 | Mesa 23+ |

## Compilación

```bash
git clone <repo>
cd mpv-wallpaper
cargo build --release
```

El binario queda en `target/release/mpv-wallpaper`.

## Uso

```bash
# Básico
./target/release/mpv-wallpaper /ruta/al/video.mp4

# O con cargo
cargo run --release -- /ruta/al/video.mp4

# Con logging más verbose
RUST_LOG=mpv_wallpaper=debug ./target/release/mpv-wallpaper video.mp4
```

### CLI Flags

| Flag | Valores | Default | Notas |
|------|---------|---------|-------|
| `-h, --help` | | | Muestra ayuda |
| `<video_path>` | ruta de archivo | requerido | Validado que existe |

## Integración con Hyprland

Añadir a `~/.config/hypr/hyprland.conf`:

```conf
# Iniciar wallpaper al arrancar Hyprland
exec-once = /ruta/a/mpv-wallpaper /ruta/al/video.mp4
```

Hyprland respeta la capa BACKGROUND de layer-shell, así que la ventana
quedará automáticamente detrás de todas las ventanas normales.

### Regla opcional para ignorar el proceso

```conf
windowrulev2 = nofocus, class:^(mpv-wallpaper)$
windowrulev2 = noshadow, class:^(mpv-wallpaper)$
```

## Formatos de video recomendados

Para bajo consumo de CPU/GPU como wallpaper:

```bash
# Convertir a H.264 optimizado para loop
ffmpeg -i original.mp4 \
  -c:v libx264 -preset slow -crf 18 \
  -an \
  -movflags +faststart \
  -vf "scale=1920:1080:flags=lanczos" \
  wallpaper.mp4

# AV1 (mejor calidad/tamaño, requiere GPU moderna para hwdec)
ffmpeg -i original.mp4 \
  -c:v libaom-av1 -crf 30 -b:v 0 \
  -an \
  wallpaper.mp4
```

## Arquitectura del código

```
main.rs (1268 líneas, archivo único)
├── FFI Bindings
│   ├── EGL (53-89)           — eglGetDisplay, eglCreateContext, etc.
│   ├── wayland-egl (95-99)   — wl_egl_window_create/destroy
│   └── mpv render (105-177)  — mpv_render_context_create/render/update
│
├── Structs
│   ├── RenderState (204-244) — EGL + mpv render ctx + Drop
│   ├── App (250-299)         — Estado global Wayland + mpv
│   └── MpvUpdateState (512)  — AtomicBool + calloop::Ping
│
├── Dispatch impls
│   ├── WlOutput (397)        — Captura dimensiones del monitor
│   ├── WlCallback (421)      — Render principal (sync con compositor)
│   └── ZwlrLayerSurface (458)— Handle Configure/Closed
│
├── Inicialización
│   ├── init_egl() (543)      — EGL/OpenGL 3.3 completo
│   ├── init_mpv() (637)      — mpv con vo=libmpv, gpu-api=opengl
│   └── create_render_context() (677) — mpv_render_context_create
│
├── Render
│   └── render_frame() (718)  — mpv_render_context_update → render → swap
│
└── main() (841-1268)
    ├── CLI parse
    ├── Wayland connect + globals
    ├── Surface + layer surface creation
    ├── EGL init → mpv init → render context
    ├── Update callback registration
    ├── loadfile + FileLoaded wait
    └── Event loop (calloop): Wayland + mpv ping + stats timer
```

## Cómo mpv renderiza (render API, sin wid)

El código usa `mpv_render_context` (render API de libmpv), **no** el enfoque `wid`:

1. mpv se inicializa con `vo=libmpv` y `gpu-api=opengl` (sin `gpu-context`)
2. La app crea un `EGLContext` de OpenGL 3.3 sobre la `wl_surface`
3. `mpv_render_context_create` recibe ese EGLContext + un callback `get_proc_address`
4. Cuando mpv tiene un frame nuevo, llama al `mpv_update_callback` que despierta el event loop
5. El event loop llama `mpv_render_context_update()` — si hay frame, `mpv_render_context_render(fbo=0)` renderiza al framebuffer de la EGLSurface
6. `eglSwapBuffers` presenta el frame al compositor
7. `mpv_render_context_report_swap` notifica a mpv que el frame fue presentado

Este enfoque da control total sobre el rendering (shaders, filtros, etc.) y es la forma recomendada de integrar mpv en aplicaciones propias.

## Limitaciones conocidas

- **Un solo monitor**: no hay lógica multi-output.

- **Sin pausa/skip**: no hay IPC ni controles de teclado por diseño.

- **Resize no implementado**: los cambios de resolución del monitor no
  redimensionan `wl_egl_window`.

## Troubleshooting

### El video no aparece / pantalla negra

```bash
# Verificar logs con debug
RUST_LOG=mpv_wallpaper=debug cargo run --release -- video.mp4 2>&1 | head -30

# Verificar que mpv funciona standalone
mpv --vo=gpu --gpu-context=wayland --no-audio video.mp4
```

### Error "zwlr_layer_shell_v1 not available"

El compositor no soporta layer-shell. Verifica que Hyprland está corriendo
y que `WAYLAND_DISPLAY` apunta al socket correcto:

```bash
echo $WAYLAND_DISPLAY
ls /run/user/$(id -u)/
```

### Hardware decoding no funciona

```bash
# Verificar vaapi (Intel/AMD)
vainfo

# Verificar nvdec (NVIDIA)
nvidia-smi

# Forzar software decoding como fallback
# Editar en main.rs: hwdec → "no"
```

### Crash al arrancar

```bash
RUST_LOG=debug cargo run -- video.mp4 2>&1 | head -50
```
