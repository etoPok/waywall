use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context;
use calloop::timer::Timer;
use tracing::{debug, warn};

use waywall::app::state::App;

#[path = "common/test_args.rs"]
mod test_args;

#[path = "common/utils.rs"]
mod utils;

fn main() -> anyhow::Result<()> {
    waywall::logging::init_test();

    let test_args = test_args::parse();
    let frames = test_args.frames;
    let mut prod_args = test_args.prod;
    let bootstrap_output = if prod_args.use_hwdec {
        waywall::app::bootstrap::bootstrap_drm(&mut prod_args)?
    } else {
        waywall::app::bootstrap::bootstrap_gl_egl(&mut prod_args)?
    };

    let (mut event_loop, mut app, loop_signal) = waywall::runtime::event_loop::build_common_loop(
        bootstrap_output.app,
        bootstrap_output.conn,
        bootstrap_output.queue,
        bootstrap_output.error_ping_source,
    )
    .context("build_common_loop")?;

    if prod_args.use_hwdec {
        event_loop
            .handle()
            .insert_source(bootstrap_output.ping_source, move |(), _, app| {
                process_drm(app, frames);
            })
            .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;
    } else {
        event_loop
            .handle()
            .insert_source(bootstrap_output.ping_source, move |(), _, app| {
                process_egl_gl(app, frames);
            })
            .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;
    }

    let grace_started = Rc::new(Cell::new(false));
    let grace_clone = grace_started.clone();
    let grace_timer = Timer::from_duration(Duration::from_millis(500));
    event_loop
        .handle()
        .insert_source(grace_timer, move |_, _, app| {
            if app.frame_count >= frames && !grace_clone.get() {
                debug!(
                    "DRM test: {} frames committed, waiting 2s for WlBuffer Release events...",
                    frames
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
                debug!(
                    "DRM test grace expired: total_buffers={}, in_use={}, free={}, frame_count={}",
                    total, in_use, free, app.frame_count
                );
                if total == frames as usize && free == 0 {
                    warn!(
                        "No WlBuffer Release received in 2s (all {} buffers still in_use). \
                         WaylandSource may not be dispatching Release correctly; check Dispatch<WlBuffer> (wayland/dispatch.rs:137).",
                        total
                    );
                } else if total == frames as usize {
                    debug!("Wayland dispatch OK: {} of {} buffers released", free, total);
                } else {
                    warn!(
                        "Unexpected buffer pool state: expected {} buffers, got {}",
                        frames, total
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

    unsafe { waywall::runtime::signals::ctrlc_setup(loop_signal) };

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in DRM event loop")?;

    drop(app);
    Ok(())
}

fn process_egl_gl(app: &mut App, max_frames: u64) {
    if app.frame_count >= max_frames {
        return;
    }

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    unsafe {
        debug!(
            "process_egl_gl: read slot pts={} pkt_dts={} best_effort={} w={} h={} fmt={} ({}) queue_len={}",
            (*frame_ptr).pts,
            (*frame_ptr).pkt_dts,
            (*frame_ptr).best_effort_timestamp,
            (*frame_ptr).width,
            (*frame_ptr).height,
            (*frame_ptr).format,
            utils::pix_fmt_name((*frame_ptr).format),
            app.frame_queue.len()
        );
    }

    use std::time::Instant;
    use waywall::timing::Timing;

    let now = Instant::now();
    let pts = unsafe { (*frame_ptr).pts };

    if app.timing.is_none()
        && let Some(ref decoder) = app.decoder
    {
        app.timing = Some(Timing::new(decoder.time_base));
        debug!("Timing initialized: time_base={}", decoder.time_base);
    }

    if let Some(last_pts) = app.last_pts
        && last_pts - 100 >= pts
        && let Some(ref decoder) = app.decoder
    {
        app.timing = Some(Timing::new(decoder.time_base));
        debug!("Timing reset after seek (pts: {} -> {})", last_pts, pts);
    }
    app.last_pts = Some(pts);

    if let Some(ref timing) = app.timing {
        if timing.should_drop(pts, now) {
            app.frame_queue.commit_read();
            app.frame_count += 1;
            warn!("Frame dropped (pts={})", pts);
            return;
        }

        let render_time = timing.render_time(pts);
        if now < render_time {
            let sleep_dur = render_time - now;
            std::thread::sleep(sleep_dur);
        }
    }

    use ffmpeg_sys_next::AVPixelFormat;
    use waywall::shader::Shader;

    let fmt = unsafe { (*frame_ptr).format as u32 };
    let gl_ctx = app.gl_ctx.as_mut().unwrap();
    let shader: &Shader = if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 as u32 {
        &gl_ctx.shader_yuv
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 as u32 {
        &gl_ctx.shader_nv12
    } else {
        warn!("Unsupported pixel format, skipping frame");
        app.frame_queue.commit_read();
        return;
    };
    unsafe {
        if gl_ctx.textures.is_empty() {
            gl_ctx.textures = waywall::render::frame::init_textures(frame_ptr);
            debug!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        waywall::render::frame::upload_frame(&gl_ctx.textures, frame_ptr);

        for rs in app.render_states.iter_mut() {
            waywall::render::egl::eglMakeCurrent(
                rs.egl_display,
                rs.egl_surface,
                rs.egl_surface,
                gl_ctx.egl_ctx,
            );

            gl::Viewport(0, 0, rs.width, rs.height);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            shader.use_program();

            let num_textures = gl_ctx.textures.len();
            for i in 0..num_textures {
                gl::ActiveTexture(gl::TEXTURE0 + i as u32);
                gl::BindTexture(gl::TEXTURE_2D, gl_ctx.textures[i]);
            }

            gl_ctx.quad.draw();

            waywall::render::egl::eglSwapBuffers(rs.egl_display, rs.egl_surface);
        }
    }

    app.frame_queue.commit_read();
    app.frame_count += 1;
}

fn process_drm(app: &mut App, max_frames: u64) {
    if app.frame_count >= max_frames {
        return;
    }

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    unsafe {
        debug!(
            "process_drm_frame: read slot pts={} pkt_dts={} best_effort={} w={} h={} fmt={} ({}) queue_len={}",
            (*frame_ptr).pts,
            (*frame_ptr).pkt_dts,
            (*frame_ptr).best_effort_timestamp,
            (*frame_ptr).width,
            (*frame_ptr).height,
            (*frame_ptr).format,
            utils::pix_fmt_name((*frame_ptr).format),
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

    if app.timing.is_none()
        && let Some(ref decoder) = app.decoder
    {
        app.timing = Some(Timing::new(decoder.time_base));
        debug!("Timing initialized: time_base={}", decoder.time_base);
    }

    let is_nop = pts == AV_NOPTS_VALUE;
    if is_nop {
        warn!("Frame with AV_NOPTS_VALUE (no pts), skipping timing");
    } else if let Some(last_pts) = app.last_pts
        && last_pts != AV_NOPTS_VALUE
        && pts < last_pts
        && let Some(ref decoder) = app.decoder
    {
        app.timing = Some(Timing::new(decoder.time_base));
        debug!("Timing reset after seek (pts: {} -> {})", last_pts, pts);
        app.last_pts = Some(pts);
    }

    if !is_nop && let Some(ref timing) = app.timing {
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
        let hw_frames_ctx = match decoder.hw_frames_ctx() {
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
        match unsafe { VaapiConverter::new(hw_frames_ctx.as_ptr(), w, h) } {
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
            app.frame_queue.commit_read();
            warn!("WlBuffer not free. Dropped frame");
            return;
        }
        Err(e) => {
            error!("{e:#}");
            app.frame_queue.commit_read();
            return;
        }
    };

    surface.attach(wbs.wl_buffer.as_ref(), 0, 0);
    surface.damage_buffer(
        0,
        0,
        wbs.drm_frame_wrapper.width,
        wbs.drm_frame_wrapper.height,
    );
    surface.commit();
    app.frame_queue.commit_read();
    app.frame_count += 1;
}
