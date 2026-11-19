use std::time::Duration;

use anyhow::Context;
use calloop::timer::Timer;
use ffmpeg_sys_next::AVFrame;
use tracing::{debug, info, warn};

use wayland_client::Proxy;
use waywall::app::state::{App, Monitor};

mod test_args;

mod utils;

type PacerTick = fn(&mut App, &fn(&mut App, *mut AVFrame)) -> calloop::timer::TimeoutAction;

fn main() -> anyhow::Result<()> {
    waywall::logging::init_test();

    let test_args = test_args::parse();
    let frames = test_args.frames;
    let bootstrap_output = if test_args.prod.use_hwdec {
        waywall::app::bootstrap::bootstrap_drm(&test_args.prod)?
    } else {
        waywall::app::bootstrap::bootstrap_gl_egl(&test_args.prod)?
    };

    let waywall::app::bootstrap::BootstrapOutput {
        mut app,
        queue,
        mut event_loop,
        compositor,
        ping_source: _,
        error_ping_source,
    } = bootstrap_output;
    waywall::runtime::event_loop::build_common_loop(
        &mut event_loop,
        &mut app,
        queue,
        compositor,
        error_ping_source,
    )
    .context("build_common_loop")?;

    let mut stop_drm_test = false;
    event_loop
        .handle()
        .insert_source(
            Timer::from_duration(Duration::from_millis(500)),
            move |_, _, app| {
                if app.committed_frames + app.dropped_frames >= frames && !stop_drm_test {
                    debug!("Waiting 2s to finish the test...",);
                    stop_drm_test = true;
                    return calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(2));
                }

                if stop_drm_test {
                    if let Some(ref signal) = app.main_loop_signal {
                        signal.stop();
                    }
                    return calloop::timer::TimeoutAction::Drop;
                }

                calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(500))
            },
        )
        .map_err(|e| anyhow::anyhow!("Error registering grace timer: {}", e))?;

    let on_frame = on_frame(test_args.prod.use_hwdec, test_args.prod.use_occlusion);
    let pacer_tick: PacerTick = if test_args.prod.use_occlusion {
        waywall::runtime::event_loop::pacer_tick_occluded
    } else {
        waywall::runtime::event_loop::pacer_tick
    };
    event_loop
        .handle()
        .insert_source(Timer::immediate(), move |_, _, app| {
            if app.committed_frames + app.dropped_frames >= frames {
                return calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(1));
            }

            pacer_tick(app, &on_frame)
        })
        .map_err(|e| anyhow::anyhow!("Error registering parcer_tick: {}", e))?;

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in event loop")?;

    info!(
        "committed_frames={}, dropped_frames={}, total_frames={}",
        app.committed_frames,
        app.dropped_frames,
        (app.dropped_frames + app.committed_frames)
    );

    if test_args.prod.use_hwdec {
        let total = app.wl_buffer_states.iter().filter(|s| s.is_some()).count();
        let in_use = app
            .wl_buffer_states
            .iter()
            .flatten()
            .filter(|wbs| wbs.in_use)
            .count();
        info!(
            "total_buffers={}/{}, in_use={}",
            total,
            app.wl_buffer_states.len(),
            in_use,
        );
    } else {
        info!(
            "skipped_frames_unhandled_wl_callback={}",
            app.skipped_frames_unhandled_wl_callback
        );
    }

    drop(app);
    Ok(())
}

fn on_frame(use_hwdec: bool, use_occlusion: bool) -> fn(&mut App, *mut AVFrame) {
    if use_hwdec && use_occlusion {
        on_drm_frame_occluded
    } else if !use_hwdec && use_occlusion {
        on_egl_gl_frame_occluded
    } else if use_hwdec {
        on_drm_frame
    } else {
        on_egl_gl_frame
    }
}

fn on_egl_gl_frame(app: &mut App, frame: *mut AVFrame) {
    render_egl_gl_frame(app, frame, |_, _| true);
}

fn on_egl_gl_frame_occluded(app: &mut App, frame: *mut AVFrame) {
    render_egl_gl_frame(app, frame, |monitors, i| {
        !monitors.get(i).is_some_and(|m| m.occluded)
    });
}

fn render_egl_gl_frame<F>(app: &mut App, frame: *mut AVFrame, should_render: F)
where
    F: Fn(&[Monitor], usize) -> bool,
{
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
    use waywall::render::shader::Shader;

    let fmt = unsafe { (*frame).format as u32 };
    let gl_ctx = app.gl_ctx.as_mut().unwrap();
    let shader: &Shader = if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 as u32 {
        &gl_ctx.shader_yuv
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 as u32 {
        &gl_ctx.shader_nv12
    } else {
        warn!("Unsupported pixel format, skipping frame");
        app.frame_queue.commit_read();
        app.dropped_frames += 1;
        return;
    };
    unsafe {
        if gl_ctx.textures.is_empty() {
            gl_ctx.textures = waywall::render::texture::init_textures(frame);
            debug!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        waywall::render::texture::upload_frame(&gl_ctx.textures, frame);

        for (i, monitor) in app.monitors.iter().enumerate() {
            if !should_render(&app.monitors, i) {
                continue;
            }

            let monitor_proxy_id = monitor.proxy.id();
            let rs = if let Some(rs) = app.render_states.get_mut(&monitor_proxy_id) {
                rs
            } else {
                continue;
            };

            if !rs.wl_callback_handled {
                app.skipped_frames_unhandled_wl_callback += 1;
                continue;
            }

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
            for t in 0..num_textures {
                gl::ActiveTexture(gl::TEXTURE0 + t as u32);
                gl::BindTexture(gl::TEXTURE_2D, gl_ctx.textures[t]);
            }

            gl_ctx.quad.draw();

            rs.surface.frame(&app.wl_conn.qh, monitor_proxy_id);
            rs.wl_callback_handled = false;

            waywall::render::egl::eglSwapBuffers(rs.egl_display, rs.egl_surface);
        }
    }

    app.frame_queue.commit_read();
    app.committed_frames += 1;
    app.frames_per_stats_sample += 1;
}

fn on_drm_frame(app: &mut App, frame: *mut AVFrame) {
    render_drm_frame(app, frame, |_, _| true);
}

fn on_drm_frame_occluded(app: &mut App, frame: *mut AVFrame) {
    render_drm_frame(app, frame, |monitors, i| {
        !monitors.get(i).is_some_and(|m| m.occluded)
    });
}

fn render_drm_frame<F>(app: &mut App, frame: *mut AVFrame, should_render: F)
where
    F: Fn(&[Monitor], usize) -> bool,
{
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
    use waywall::render::drm::converter::VaapiConverter;

    if app.converter.is_none() {
        let w = unsafe { (*frame).width };
        let h = unsafe { (*frame).height };
        let hw_frames_ctx = match app.decoder.hw_frames_ctx() {
            Some(f) => f,
            None => {
                error!("VAAPI converter: hw_frames_ctx not available");
                app.frame_queue.commit_read();
                if let Some(ref signal) = app.main_loop_signal {
                    signal.stop();
                }
                return;
            }
        };
        match unsafe {
            VaapiConverter::create_nv12_to_bgra_graph_filter(hw_frames_ctx.as_ptr(), w, h)
        } {
            Ok(converter) => {
                app.converter = Some(converter);
            }
            Err(e) => {
                error!("Failed to create VAAPI converter: {e:#}");
                app.frame_queue.commit_read();
                if let Some(ref signal) = app.main_loop_signal {
                    signal.stop();
                }
                return;
            }
        }
    }

    let (buf, buf_w, buf_h) = {
        let wbs = match app.get_or_create_wl_buffer(unsafe { &mut *frame }) {
            Ok(Some(wbs)) => wbs,
            Ok(None) => {
                app.frame_queue.commit_read();
                app.dropped_frames += 1;
                warn!("WlBuffer not free. Dropped frame");
                return;
            }
            Err(e) => {
                error!("{e:#}");
                app.frame_queue.commit_read();
                return;
            }
        };
        (
            wbs.wl_buffer.as_ref().unwrap().clone(),
            wbs.drm_frame_wrapper.width,
            wbs.drm_frame_wrapper.height,
        )
    };

    for (i, monitor) in app.monitors.iter().enumerate() {
        if !should_render(&app.monitors, i) {
            continue;
        }

        let surface = monitor.surface.as_ref().unwrap();
        surface.attach(Some(&buf), 0, 0);
        surface.damage_buffer(0, 0, buf_w, buf_h);
        surface.commit();
    }

    app.frame_queue.commit_read();
    app.committed_frames += 1;
    app.frames_per_stats_sample += 1;
}
