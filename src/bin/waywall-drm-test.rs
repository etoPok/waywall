use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context;
use calloop::timer::Timer;
use tracing::{info, warn};

use waywall::app::state::App;

const DRM_TEST_FRAMES: u64 = 60;

fn main() -> anyhow::Result<()> {
    waywall::logging::init_test();

    let mut args = waywall::cli::args::parse();
    let out = waywall::app::bootstrap::bootstrap_drm_pipeline(&mut args)?;

    let (mut event_loop, mut app, loop_signal) = waywall::runtime::event_loop::build_common_loop(
        out.app,
        out.conn,
        out.queue,
        out.error_ping_source,
    )
    .context("build_common_loop")?;

    event_loop
        .handle()
        .insert_source(out.ping_source, |(), _, app| {
            process_drm_test(app);
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;

    let grace_started = Rc::new(Cell::new(false));
    let grace_clone = grace_started.clone();
    let grace_timer = Timer::from_duration(Duration::from_millis(500));
    event_loop
        .handle()
        .insert_source(grace_timer, move |_, _, app| {
            if app.frame_count >= DRM_TEST_FRAMES && !grace_clone.get() {
                info!(
                    "DRM test: {} frames committed, waiting 2s for WlBuffer Release events...",
                    DRM_TEST_FRAMES
                );
                grace_clone.set(true);
                return calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(2));
            }
            if grace_clone.get() {
                let total = app.wl_buffer_states.iter().filter(|s| s.is_some()).count();
                let free = app
                    .wl_buffer_states
                    .iter()
                    .flatten()
                    .filter(|wbs| !wbs.in_use)
                    .count();
                let in_use = total.saturating_sub(free);
                info!(
                    "DRM test grace expired: total_buffers={}, in_use={}, free={}, frame_count={}",
                    total, in_use, free, app.frame_count
                );
                if total == DRM_TEST_FRAMES as usize && free == 0 {
                    warn!(
                        "No WlBuffer Release received in 2s (all {} buffers still in_use). \
                         WaylandSource may not be dispatching Release correctly; check Dispatch<WlBuffer> (wayland/dispatch.rs:137).",
                        total
                    );
                } else if total == DRM_TEST_FRAMES as usize {
                    info!("Wayland dispatch OK: {} of {} buffers released", free, total);
                } else {
                    warn!(
                        "Unexpected buffer pool state: expected {} buffers, got {}",
                        DRM_TEST_FRAMES, total
                    );
                }
                if let Some(ref signal) = app.loop_signal {
                    signal.stop();
                }
                return calloop::timer::TimeoutAction::Drop;
            }
            calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(500))
        })
        .map_err(|e| anyhow::anyhow!("Error registering DRM grace timer: {}", e))?;

    app.last_stats_time = Some(std::time::Instant::now());
    info!(
        "DRM test event loop started (target {} frames). Ctrl+C to exit.",
        DRM_TEST_FRAMES
    );

    unsafe { waywall::runtime::signals::ctrlc_setup(loop_signal) };

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in DRM event loop")?;

    drop(app);
    info!("Clean exit (DRM test).");
    Ok(())
}

fn process_drm_test(app: &mut App) {
    if app.frame_count >= DRM_TEST_FRAMES {
        return;
    }

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    unsafe {
        info!(
            "process_drm_frame: read slot pts={} pkt_dts={} best_effort={} w={} h={} fmt={} queue_len={}",
            (*frame_ptr).pts,
            (*frame_ptr).pkt_dts,
            (*frame_ptr).best_effort_timestamp,
            (*frame_ptr).width,
            (*frame_ptr).height,
            (*frame_ptr).format,
            app.frame_queue.len()
        );
    }

    use std::time::Instant;
    use tracing::{error, warn};
    use waywall::timing::Timing;
    use waywall::vaapi_converter::VaapiConverter;

    let now = Instant::now();
    const AV_NOPTS_VALUE: i64 = 0x8000000000000000u64 as i64;
    let pts = unsafe { (*frame_ptr).pts };
    let is_nop = pts == AV_NOPTS_VALUE;
    if is_nop {
        warn!("Frame with AV_NOPTS_VALUE (no pts), skipping timing");
    }

    if app.timing.is_none() {
        if let Some(ref decoder) = app.decoder {
            app.timing = Some(Timing::new(decoder.time_base));
            info!("Timing initialized: time_base={}", decoder.time_base);
        }
    }

    if let Some(last_pts) = app.last_pts {
        if !is_nop && last_pts != AV_NOPTS_VALUE && pts < last_pts - 100 {
            if let Some(ref decoder) = app.decoder {
                app.timing = Some(Timing::new(decoder.time_base));
                info!("Timing reset after seek (pts: {} -> {})", last_pts, pts);
            }
        }
    }
    if !is_nop {
        app.last_pts = Some(pts);
    }

    if let Some(ref timing) = app.timing {
        if !is_nop {
            if timing.should_drop(pts, now) {
                app.frame_queue.commit_read();
                warn!("Frame dropped (pts={})", pts);
                return;
            }
            let render_time = timing.render_time(pts);
            if now < render_time {
                let sleep_dur = render_time - now;
                std::thread::sleep(sleep_dur);
            }
        }
    }

    if app.converter.is_none() {
        let w = unsafe { (*frame_ptr).width };
        let h = unsafe { (*frame_ptr).height };
        let decoder = match app.decoder.as_ref() {
            Some(d) => d,
            None => {
                error!("VAAPI converter: no decoder available");
                app.frame_queue.commit_read();
                if let Some(ref signal) = app.loop_signal {
                    signal.stop();
                }
                return;
            }
        };
        let frames = match decoder.hw_frames_ctx() {
            Some(f) => f,
            None => {
                error!("VAAPI converter: hw_frames_ctx not available");
                app.frame_queue.commit_read();
                if let Some(ref signal) = app.loop_signal {
                    signal.stop();
                }
                return;
            }
        };
        match unsafe { VaapiConverter::new(frames.as_ptr(), w, h) } {
            Ok(converter) => {
                app.converter = Some(converter);
            }
            Err(e) => {
                error!("Failed to create VAAPI converter: {e:#}");
                app.frame_queue.commit_read();
                if let Some(ref signal) = app.loop_signal {
                    signal.stop();
                }
                return;
            }
        }
    }

    let surface = app.monitors[0].surface.as_ref().unwrap().clone();
    let wbs = match app.acquire_or_create_buffer(unsafe { &mut *frame_ptr }) {
        Ok(Some(wbs)) => wbs,
        Ok(None) => {
            warn!("No free WlBuffer slot and no reusable buffer, dropping frame");
            app.frame_queue.commit_read();
            return;
        }
        Err(e) => {
            error!("{e:#}");
            app.frame_queue.commit_read();
            return;
        }
    };

    surface.attach(wbs.wl_buffer.as_ref(), 0, 0);
    surface.damage_buffer(0, 0, wbs.drm_frame_wrapper.width, 1088);
    surface.commit();
    app.frame_queue.commit_read();
    app.frame_count += 1;
}
