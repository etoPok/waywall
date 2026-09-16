use std::fs;
use std::os::linux::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::debug;

/// Converts the main_device byte array into a render node.
pub fn render_node_from_main_device(device: &[u8]) -> Result<PathBuf> {
    if device.is_empty() {
        bail!("main_device is empty");
    }

    if let Ok(dev) = parse_dev_t(device) {
        // look it up directly by st_rdev in /dev/dri.
        if let Some(path) = find_drm_node_by_dev_t(dev) {
            return ensure_render_node(&path);
        }

        // fallback: look up by major:minor in sysfs.
        let major = major_of(dev);
        let minor = minor_of(dev);

        if let Some(path) = find_drm_node_by_major_minor(major, minor) {
            let render_node = ensure_render_node(&path)?;
            debug!("render node obtained: {:?}", render_node);
            return Ok(render_node);
        }
    }

    bail!("No /dev/dri node matching main_device ({:?}) found", device)
}

/// Interprets bytes as a native dev_t.
fn parse_dev_t(bytes: &[u8]) -> Result<u64> {
    match bytes.len() {
        8 => {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(bytes);
            Ok(u64::from_ne_bytes(arr))
        }
        4 => {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(bytes);
            Ok(u32::from_ne_bytes(arr) as u64)
        }
        n => bail!("Unexpected length for dev_t: {n} bytes"),
    }
}

/// Finds a /dev/dri node whose st_rdev matches dev.
fn find_drm_node_by_dev_t(dev: u64) -> Option<PathBuf> {
    let entries = fs::read_dir("/dev/dri").ok()?;

    for entry in entries.flatten() {
        let path = entry.path();

        if let Ok(meta) = fs::metadata(&path)
            && meta.st_rdev() == dev
        {
            return Some(path);
        }
    }

    None
}

/// Searches /sys/class/drm for a device whose `dev` file is "major:minor".
fn find_drm_node_by_major_minor(major: u64, minor: u64) -> Option<PathBuf> {
    let expected = format!("{major}:{minor}");

    let entries = fs::read_dir("/sys/class/drm").ok()?;

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip connectors like card0-HDMI-A-1.
        if name.contains('-') {
            continue;
        }

        let dev_file = entry.path().join("dev");

        if let Ok(content) = fs::read_to_string(dev_file)
            && content.trim() == expected
        {
            let path = Path::new("/dev/dri").join(name.as_ref());

            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

/// If the device is already renderD*, it is used directly.
/// If it is card*, an attempt is made to map it to its render node.
fn ensure_render_node(path: &Path) -> Result<PathBuf> {
    if is_render_node(path) {
        return Ok(path.to_path_buf());
    }

    render_node_from_drm_node(path)
}

fn is_render_node(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.starts_with("renderD"))
        .unwrap_or(false)
}

/// Given /dev/dri/cardX or another DRM node, finds its associated render node.
fn render_node_from_drm_node(node: &Path) -> Result<PathBuf> {
    let name = node
        .file_name()
        .and_then(|s| s.to_str())
        .context("DRM node has no file name")?;

    let sys_device_path = Path::new("/sys/class/drm").join(name).join("device");

    let sys_device = fs::canonicalize(&sys_device_path)
        .with_context(|| format!("Failed to canonicalize {:?}", sys_device_path))?;

    if let Ok(render) = render_node_from_sys_device(&sys_device) {
        return Ok(render);
    }

    // Simple fallback: card0 -> renderD128, card1 -> renderD129, etc.
    if let Some(card_number) = name.strip_prefix("card")
        && let Ok(n) = card_number.parse::<u32>()
    {
        let render_name = format!("renderD{}", 128 + n);
        let render_path = Path::new("/dev/dri").join(render_name);

        if render_path.exists() {
            return Ok(render_path);
        }
    }

    bail!("Failed to map {:?} to a renderD* node", node)
}

/// Finds the render node whose /sys/class/drm/renderD*/device points to the same
/// sysfs device.
fn render_node_from_sys_device(sys_device: &Path) -> Result<PathBuf> {
    let sys_device = fs::canonicalize(sys_device)
        .with_context(|| format!("Failed to canonicalize {:?}", sys_device))?;

    let entries = fs::read_dir("/sys/class/drm").context("Failed to read /sys/class/drm")?;

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if !name.starts_with("renderD") {
            continue;
        }

        let device_path = Path::new("/sys/class/drm")
            .join(name.as_ref())
            .join("device");

        if let Ok(candidate_device) = fs::canonicalize(device_path)
            && candidate_device == sys_device
        {
            return Ok(Path::new("/dev/dri").join(name.as_ref()));
        }
    }

    bail!("No render node found for sysfs device {:?}", sys_device)
}

/// Extracts the `major` number from a Linux `dev_t`.
///
/// Linux does not store `major:minor` as plain fields. `dev_t` is an
/// opaque 64-bit encoding defined by glibc `sys/sysmacros.h`
/// (`makedev`/`major`/`minor`, see `man 3 makedev`):
///
/// ```text
/// dev = ((major & 0xFFF) << 8)
///     | ((major & 0xFFFFF000) << 32)
///     | ((minor & 0xFF) << 0)
///     | ((minor & 0xFFFFFF00) << 12)
/// ```
///
/// This is the inverse of that encoding.
fn major_of(dev: u64) -> u64 {
    ((dev >> 8) & 0xfff_u64) | ((dev >> 32) & 0xfffff000_u64)
}

/// Extracts the `minor` number from a Linux `dev_t`.
///
/// Inverse of `makedev(3)`, see [`major_of`] for the bit layout.
fn minor_of(dev: u64) -> u64 {
    (dev & 0xff_u64) | ((dev >> 12) & 0xffffff00_u64)
}
