use std::time::{Duration, Instant};

use anyhow::Context;
use calloop::ping::PingSource;
use calloop::timer::Timer;
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use ffmpeg_sys_next::AVPixelFormat;
use tracing::{error, info, warn};
use wayland_client::{Connection, EventQueue};

use crate::app::state::App;
use crate::shader::Shader;
use crate::timing::Timing;
use crate::vaapi_converter::VaapiConverter;

const DRM_TEST_FRAMES: u64 = 60;

pub fn run(
    mut app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
    error_ping_source: PingSource,
) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<App> =
        EventLoop::try_new().context("Error creating event loop")?;

    let loop_signal = event_loop.get_signal();
    app.loop_signal = Some(loop_signal.clone());

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registering Wayland source in event loop: {}", e))?;

    // PingSource — fires once per decoder frame commit
    event_loop
        .handle()
        .insert_source(ping_source, |(), _, app| {
            process_frame(app);
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;

    // ErrorPingSource — fires once when decoder encounters a fatal error
    event_loop
        .handle()
        .insert_source(error_ping_source, |(), _, app| {
            if let Some(ref signal) = app.loop_signal {
                signal.stop();
            }
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder error ping: {}", e))?;

    let stats_timer = Timer::from_duration(Duration::from_secs(5));
    event_loop
        .handle()
        .insert_source(stats_timer, |_, _, app| {
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
            info!("Stats: {:.1} fps, {} frames", fps, frames);
            app.frame_count = 0;
            app.last_stats_time = Some(Instant::now());
            calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(5))
        })
        .map_err(|e| anyhow::anyhow!("Error registering stats timer: {}", e))?;

    app.last_stats_time = Some(Instant::now());

    info!("Event loop started. Ctrl+C to exit.");

    unsafe { crate::runtime::signals::ctrlc_setup(loop_signal) };

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in event loop")?;

    drop(app);

    info!("Clean exit.");
    Ok(())
}

pub fn run_drm(
    mut app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
    error_ping_source: PingSource,
) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<App> =
        EventLoop::try_new().context("Error creating event loop")?;

    let loop_signal = event_loop.get_signal();
    app.loop_signal = Some(loop_signal.clone());

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registering Wayland source in event loop: {}", e))?;

    event_loop
        .handle()
        .insert_source(ping_source, |(), _, app| {
            process_drm_frame(app);
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;

    event_loop
        .handle()
        .insert_source(error_ping_source, |(), _, app| {
            if let Some(ref signal) = app.loop_signal {
                signal.stop();
            }
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder error ping: {}", e))?;

    let grace_started = std::rc::Rc::new(std::cell::Cell::new(false));
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

    app.last_stats_time = Some(Instant::now());

    info!(
        "DRM test event loop started (target {} frames). Ctrl+C to exit.",
        DRM_TEST_FRAMES
    );

    unsafe { crate::runtime::signals::ctrlc_setup(loop_signal) };

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in DRM event loop")?;

    drop(app);

    info!("Clean exit (DRM test).");
    Ok(())
}

fn process_frame(app: &mut App) {
    let now = Instant::now();

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    let pts = unsafe { (*frame_ptr).pts };

    if app.timing.is_none() {
        if let Some(ref decoder) = app.decoder {
            app.timing = Some(Timing::new(decoder.time_base));
            info!("Timing initialized: time_base={}", decoder.time_base);
        }
    }

    if let Some(last_pts) = app.last_pts {
        if pts < last_pts - 100 {
            if let Some(ref decoder) = app.decoder {
                app.timing = Some(Timing::new(decoder.time_base));
                info!("Timing reset after seek (pts: {} -> {})", last_pts, pts);
            }
        }
    }
    app.last_pts = Some(pts);

    if let Some(ref timing) = app.timing {
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

    let fmt = unsafe { (*frame_ptr).format as u32 };

    let shader: &Shader;
    if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 as u32 {
        shader = app.shader_yuv.as_ref().unwrap();
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 as u32 {
        shader = app.shader_nv12.as_ref().unwrap();
    } else {
        warn!("Unsupported pixel format, skipping frame");
        app.frame_queue.commit_read();
        return;
    }

    let quad = app.quad.as_ref().unwrap();

    unsafe {
        for rs in app.render_states.iter_mut() {
            crate::render::egl::eglMakeCurrent(
                rs.egl_display,
                rs.egl_surface,
                rs.egl_surface,
                rs.egl_context,
            );

            if rs.textures.is_empty() {
                rs.textures = crate::render::frame::init_textures(rs, frame_ptr);
                info!(
                    "Textures created for monitor ({} textures)",
                    rs.textures.len()
                );
            }

            crate::render::frame::upload_frame(&rs.textures, frame_ptr);

            gl::Viewport(0, 0, rs.width, rs.height);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            shader.use_program();

            let num_textures = rs.textures.len();
            for i in 0..num_textures {
                gl::ActiveTexture(gl::TEXTURE0 + i as u32);
                gl::BindTexture(gl::TEXTURE_2D, rs.textures[i]);
            }

            quad.draw();

            crate::render::egl::eglSwapBuffers(rs.egl_display, rs.egl_surface);
        }
    }

    app.frame_queue.commit_read();
    app.frame_count += 1;
}

fn process_drm_frame(app: &mut App) {
    if app.frame_count >= DRM_TEST_FRAMES {
        return;
    }
    let now = Instant::now();

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
        match VaapiConverter::new(app.decoder.as_ref().unwrap().hw_device_ctx, w, h) {
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
    let wbs = match app.acquire_or_create_buffer(frame_ptr) {
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
