use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Context;
use calloop::{ping, timer::Timer, EventLoop};
use calloop_wayland_source::WaylandSource;
use tracing::{info, warn};
use wayland_client::{Connection, EventQueue};

use crate::app::state::App;
use crate::bindings::egl::eglMakeCurrent;
use crate::bindings::mpv::mpv_render_context_set_update_callback;
use crate::mpv::callbacks::noop_update_callback;
use crate::mpv::events::process_mpv_events;
use crate::render::frame::{has_new_frame, render_frame};
use crate::runtime::signals::ctrlc_setup;

pub fn run(
    mut app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: ping::PingSource,
) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<App> =
        EventLoop::try_new().context("Error creando event loop")?;

    let loop_signal = event_loop.get_signal();
    app.loop_signal = Some(loop_signal.clone());

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registrando fuente Wayland en event loop: {}", e))?;

    // Insert PingSource: wakes the event loop when mpv calls the update callback.
    event_loop
        .handle()
        .insert_source(ping_source, |(), &mut (), _| {})
        .map_err(|e| anyhow::anyhow!("Error registrando PingSource en event loop: {}", e))?;

    let stats_timer = Timer::from_duration(Duration::from_secs(5));
    event_loop
        .handle()
        .insert_source(stats_timer, |_, _, app| {
            if let Some(mpv) = &app.mpv {
                let frames = app.frame_count;
                let elapsed = app
                    .last_stats_time
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(5.0);
                let fps = if elapsed > 0.0 {
                    frames as f64 / elapsed
                } else {
                    0.0
                };

                // Query decoder frame-drop count.
                if let Ok(val) = mpv.get_property::<i64>("decoder-frame-drop-count") {
                    if val > 0 {
                        warn!(
                            "Stats: {:.1} fps, {} frames, decoder drops: {}",
                            fps, frames, val
                        );
                    } else {
                        info!("Stats: {:.1} fps, {} frames, no drops", fps, frames);
                    }
                } else {
                    info!("Stats: {:.1} fps, {} frames", fps, frames);
                }

                // Query estimated video fps.
                if let Ok(val) = mpv.get_property::<f64>("estimated-vf-fps") {
                    info!("  estimated-vf-fps: {:.2}", val);
                }
            }
            app.frame_count = 0;
            app.last_stats_time = Some(Instant::now());
            // reschedule timer
            calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(5))
        })
        .map_err(|e| anyhow::anyhow!("Error registrando stats timer: {}", e))?;

    app.last_stats_time = Some(Instant::now());

    info!("Event loop started (no polling). Ctrl+C to exit.");
    unsafe { ctrlc_setup(loop_signal) };

    // Sleeps indefinitely until mpv or Wayland wake the loop.
    // No periodic timer — CPU usage ≈ 0 when there are no frames.
    event_loop
        .run(None, &mut app, |app| {
            // are there mpv events that must be processed in response to Wayland events?
            if let Some(mpv) = &mut app.mpv {
                process_mpv_events(mpv, &app.loop_signal);
            }

            // First frame: request frame callback BEFORE rendering so that
            // eglSwapBuffers commits the surface including the frame request.
            if !app.first_render_attempted {
                app.first_render_attempted = true;
                for monitor in app.monitors.iter_mut() {
                    if let Some(surface) = &monitor.surface {
                        if let Some(qh) = &app.qh {
                            monitor.wl_callback = Some(surface.frame(qh, ()));
                        }
                    }
                }
                app.pending_wl_callbacks = app.monitors.len();

                let has_new_frame = unsafe { has_new_frame(app.mpv_render_ctx) };
                unsafe {
                    for rs in app.render_states.iter_mut() {
                        eglMakeCurrent(
                            rs.egl_display,
                            rs.egl_surface,
                            rs.egl_surface,
                            rs.egl_context,
                        );
                        if render_frame(rs, app.mpv_render_ctx, has_new_frame) {
                            app.frame_count += 1;
                        }
                    }
                }
            }

            // When mpv has new data (mpv_update_callback), request
            // a Wayland frame. The REAL render happens in Dispatch<WlCallback>
            // (vsync), where render_frame calls mpv_render_context_update which
            // rearms the callback for the next frame.
            let needs_render = app
                .mpv_update_state
                .map(|ptr| unsafe { (*ptr).needs_update.swap(false, Ordering::SeqCst) })
                .unwrap_or(false);

            if needs_render && app.pending_wl_callbacks == 0 {
                for monitor in app.monitors.iter_mut() {
                    if let Some(surface) = &monitor.surface {
                        if let Some(qh) = &app.qh {
                            monitor.wl_callback = Some(surface.frame(qh, ()));
                        }
                    }
                }
                app.pending_wl_callbacks = app.monitors.len();
            }
        })
        .context("Error en event loop")?;

    // ------------------------------------------------------------------
    // Cleanup
    // ------------------------------------------------------------------

    info!("Exiting cleanly...");

    unsafe {
        mpv_render_context_set_update_callback(
            app.mpv_render_ctx,
            noop_update_callback,
            std::ptr::null_mut(),
        );
    }

    drop(app);

    info!("Clean exit.");
    Ok(())
}
