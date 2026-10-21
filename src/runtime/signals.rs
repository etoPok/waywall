use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use calloop::LoopSignal;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use tracing::info;

pub static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: std::ffi::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}

pub fn ctrlc_setup(loop_signal: LoopSignal) -> Result<()> {
    let action = SigAction::new(
        SigHandler::Handler(handle_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );

    unsafe {
        sigaction(Signal::SIGINT, &action)?;
        sigaction(Signal::SIGTERM, &action)?;
    }

    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if TERMINATE.load(Ordering::Relaxed) {
                info!("Signal received, shutting down...");
                loop_signal.stop();
                break;
            }
        }
    });

    Ok(())
}
