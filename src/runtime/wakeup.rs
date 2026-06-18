use std::sync::atomic::AtomicBool;

use calloop::ping;

/// Shared state between the mpv callback (decoding thread)
/// and the main event loop.
pub struct MpvUpdateState {
    pub needs_update: AtomicBool,
    pub ping: ping::Ping,
}
