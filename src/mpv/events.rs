use calloop::LoopSignal;
use libmpv2::events::Event;
use libmpv2::Mpv;
use tracing::{debug, error, warn};

use crate::bindings::mpv::mpv_error_string;

/// Converts an mpv error to its textual description using mpv_error_string.
pub fn fmt_mpv_error(e: &libmpv2::Error) -> String {
    match e {
        libmpv2::Error::Raw(code) => {
            let s = unsafe {
                let ptr = mpv_error_string(*code);
                if ptr.is_null() {
                    format!("Raw({}) (unknown)", code)
                } else {
                    let cstr = std::ffi::CStr::from_ptr(ptr);
                    format!("Raw({}): {}", code, cstr.to_string_lossy())
                }
            };
            s
        }
        _ => format!("{}", e),
    }
}

pub fn process_mpv_events(mpv: &mut Mpv, loop_signal: &Option<LoopSignal>) {
    loop {
        match mpv.event_context_mut().wait_event(0.0) {
            Some(Ok(Event::EndFile(reason))) => {
                warn!("mpv: EndFile ({:?}), the loop should restart", reason);
            }
            Some(Ok(Event::Shutdown)) => {
                error!("mpv closed unexpectedly");
                if let Some(signal) = loop_signal {
                    signal.stop();
                }
                break;
            }
            Some(Ok(Event::LogMessage { text, .. })) => {
                debug!("mpv: {}", text.trim());
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                error!("Error in mpv event: {}", fmt_mpv_error(&e));
                break;
            }
            None => break,
        }
    }
}
