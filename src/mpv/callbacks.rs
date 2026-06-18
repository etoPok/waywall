use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::debug;

use crate::runtime::wakeup::MpvUpdateState;

pub static UPDATE_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

pub extern "C" fn mpv_update_callback(ctx: *mut c_void) {
    let count = UPDATE_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    debug!("mpv_update_callback called {} times", count + 1);

    unsafe {
        let state = &*(ctx as *const MpvUpdateState);
        state.needs_update.store(true, Ordering::SeqCst);
        state.ping.ping();
    }
}

/// empty callback to unregister the mpv update callback during cleanup
pub extern "C" fn noop_update_callback(_ctx: *mut c_void) {}
