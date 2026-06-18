use std::os::raw::c_void;

use wayland_client::Proxy;

/// Returns the native *mut wl_proxy of any Proxy.
///
/// Requires in Cargo.toml:
///   wayland-backend = { version = "0.3", features = ["client_system"] }
pub fn proxy_to_raw_ptr<P: Proxy>(proxy: &P) -> *mut c_void {
    // wayland_backend::ObjectId::as_ptr() returns the native *mut wl_proxy.
    // This is the public and stable sys backend API.
    proxy.id().as_ptr() as *mut c_void
}
