# mpvwall

mpvwall is a Wayland video wallpaper client for compositors supporting the wlr-layer-shell protocol, such as Hyprland and Sway.

<https://github.com/user-attachments/assets/99ecf992-db35-4fcb-811c-7cd1131fb6b8>

## System dependencies

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

Verify that pkg-config finds libmpv:

```bash
pkg-config --modversion mpv
# Should print something like: 0.37.0
```

## Compilation

```bash
git clone <repo>
cd mpvwall
cargo build --release
```

## Usage

```bash
# Basic
./target/release/mpvwall /path/to/video.mp4

# Or with cargo
cargo run --release -- /path/to/video.mp4

# With more verbose logging
RUST_LOG=mpv_wallpaper=debug ./target/release/mpvwall video.mp4
```

### CLI Flags

| Flag | Values | Default | Notes |
|------|--------|---------|-------|
| `-h, --help` | | | Shows help |
| `<video_path>` | file path | required | Validated that it exists |

## Hyprland integration

Add to `~/.config/hypr/hyprland.conf`:

```conf
# Start wallpaper when Hyprland boots
exec-once = /path/to/mpvwall /path/to/video.mp4
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

## Troubleshooting

### Video does not appear / black screen

```bash
# Check logs with debug
RUST_LOG=mpv_wallpaper=debug cargo run --release -- video.mp4 2>&1 | head -30

# Verify that mpv works standalone
mpv --vo=gpu --gpu-context=wayland --no-audio video.mp4
```

### Error "zwlr_layer_shell_v1 not available"

The compositor does not support layer-shell. Verify that `WAYLAND_DISPLAY` points to the correct socket:

```bash
echo $WAYLAND_DISPLAY
ls /run/user/$(id -u)/
```

## License
This project is licensed under the GPLv3 License. See the [LICENSE](./LICENSE) file for details.
