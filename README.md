# waywall

waywall is a Wayland video wallpaper client for compositors supporting the wlr-layer-shell protocol, such as Hyprland and Sway.

<https://github.com/user-attachments/assets/99ecf992-db35-4fcb-811c-7cd1131fb6b8>

## Dependencies

Requires a Wayland compositor with GPU support. Only FFmpeg needs to be installed explicitly:

```bash
# Arch Linux
sudo pacman -S ffmpeg

# Ubuntu 24.04 / Debian Bookworm
sudo apt install ffmpeg

# Fedora
sudo dnf install ffmpeg
```

Build dependencies:

```bash
# Arch Linux
sudo pacman -S base-devel pkg-config ffmpeg

# Ubuntu / Debian
sudo apt install pkg-config build-essential libavformat-dev libavcodec-dev libavutil-dev

# Fedora
sudo dnf install gcc pkg-config ffmpeg-devel
```

## Compilation

```bash
git clone <repo>
cd waywall
cargo build --release
```

## Usage

```bash
# Basic
waywall /path/to/video.mp4

# Specify a single output
waywall -o eDP-1 /path/to/video.mp4

# Specify multiple outputs
./target/release/waywall -o eDP-1,DP-3 /path/to/video.mp4

# Hardware-accelerated decoding via VA-API
waywall -o eDP-1 --vaapi /path/to/video.mp4
```

### CLI Flags

| Flag | Values | Default | Notes |
|------|--------|---------|-------|
| `-h, --help` | | | Shows help |
| `-o, --output` | connector name | all outputs | Can be repeated or comma-separated (e.g. `-o eDP-1,DP-3`) |
| `--vaapi` | | disabled | Enables VA-API hardware decoding; resolves the compositor render node from linux-dmabuf feedback (`zwp_linux_dmabuf_v1` v4) and runs the DRM pipeline. Falls back to software/GL with a warning if dmabuf is unavailable. Requires a VA-API capable FFmpeg/driver and `/dev/dri/renderD*` access |
| `--no-gl` | | EGL enabled | Disables OpenGL/EGL rendering |
| `<video_path>` | file path | required | Validated that it exists |

## Hyprland integration

Add to `~/.config/hypr/hyprland.conf`:

```conf
# Start wallpaper when Hyprland boots
exec-once = /path/to/waywall /path/to/video.mp4
```

## Recommended video formats

For low CPU/GPU usage as wallpaper:

```bash
# Convert to H.264 optimized for loop
ffmpeg -i original.mp4 \
  -c:v libx264 -preset slow -crf 18 \
  -an \
  -movflags +faststart \
  -vf "scale=1920:1080:flags=lanczos" \
  wallpaper.mp4

# AV1 (better quality/size, requires modern GPU for hwdec)
ffmpeg -i original.mp4 \
  -c:v libaom-av1 -crf 30 -b:v 0 \
  -an \
  wallpaper.mp4
```

## Known limitations

- **Resize not implemented**: monitor resolution changes do not
  resize `wl_egl_window`.

- **4K 60fps video performance**: videos running at 3840×2160 at 60 fps
  may exhibit indefinite frame loss

## Troubleshooting

### Video does not appear / black screen

```bash
# Check logs with debug
RUST_LOG=waywall=debug cargo run --release -- video.mp4 2>&1 | head -30
```

### Error "zwlr_layer_shell_v1 not available"

The compositor does not support layer-shell. Verify that `WAYLAND_DISPLAY` points to the correct socket:

```bash
echo $WAYLAND_DISPLAY
ls /run/user/$(id -u)/
```

## License
This project is licensed under the GPLv3 License. See the [LICENSE](./LICENSE) file for details.
