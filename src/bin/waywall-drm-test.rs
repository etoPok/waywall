use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context;
use calloop::timer::Timer;
use ffmpeg_sys_next::AVFrame;
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
                if app.frame_count > frames {
                    return;
                }
                waywall::runtime::event_loop::common_loop(app, &on_drm_frame);
            })
            .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;
    } else {
        event_loop
            .handle()
            .insert_source(bootstrap_output.ping_source, move |(), _, app| {
                if app.frame_count > frames {
                    return;
                }
                waywall::runtime::event_loop::common_loop(app, &on_egl_gl_frame);
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

fn on_egl_gl_frame(app: &mut App, frame: *mut AVFrame) {
    unsafe {
        debug!(
            "process_egl_gl: read slot pts={} pkt_dts={} best_effort={} w={} h={} fmt={} ({}) queue_len={}",
            (*frame).pts,
            (*frame).pkt_dts,
            (*frame).best_effort_timestamp,
            (*frame).width,
            (*frame).height,
            (*frame).format,
            utils::pix_fmt_name((*frame).format),
            app.frame_queue.len()
        );
    }

    use ffmpeg_sys_next::AVPixelFormat;
    use waywall::shader::Shader;

    let fmt = unsafe { (*frame).format as u32 };
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
            gl_ctx.textures = waywall::render::frame::init_textures(frame);
            debug!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        waywall::render::frame::upload_frame(&gl_ctx.textures, frame);

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

fn on_drm_frame(app: &mut App, frame: *mut AVFrame) {
    unsafe {
        debug!(
            "process_drm_frame: read slot pts={} pkt_dts={} best_effort={} w={} h={} fmt={} ({}) queue_len={}",
            (*frame).pts,
            (*frame).pkt_dts,
            (*frame).best_effort_timestamp,
            (*frame).width,
            (*frame).height,
            (*frame).format,
            utils::pix_fmt_name((*frame).format),
            app.frame_queue.len()
        );
    }

    use tracing::{error, warn};
    use waywall::vaapi_converter::VaapiConverter;

    if app.converter.is_none() {
        let w = unsafe { (*frame).width };
        let h = unsafe { (*frame).height };
        let hw_frames_ctx = match app.decoder.hw_frames_ctx() {
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
    let wbs = match app.acquire_or_create_buffer(unsafe { &mut *frame }) {
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
