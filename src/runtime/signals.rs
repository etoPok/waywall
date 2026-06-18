use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calloop::LoopSignal;
use tracing::info;

pub static TERMINATE: AtomicBool = AtomicBool::new(false);

pub unsafe fn ctrlc_setup(loop_signal: LoopSignal) {
    extern "C" fn handle_signal(_sig: libc::c_int) {
        TERMINATE.store(true, Ordering::Relaxed);
    }
    type SigFn = unsafe extern "C" fn(libc::c_int);
    let handler = handle_signal as SigFn as libc::sighandler_t;
    libc::signal(libc::SIGINT, handler);
    libc::signal(libc::SIGTERM, handler);

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        if TERMINATE.load(Ordering::Relaxed) {
            info!("Signal received, shutting down...");
            loop_signal.stop();
            break;
        }
    });
}
