use std::time::{Duration, Instant};

use anyhow::Context;
use calloop::ping::PingSource;
use calloop::timer::Timer;
use calloop::{EventLoop, LoopSignal};
use calloop_wayland_source::WaylandSource;
use ffmpeg_sys_next::AVPixelFormat;
use tracing::{error, info, warn};
use wayland_client::{Connection, EventQueue};

use crate::app::state::App;
use crate::shader::Shader;
use crate::timing::Timing;
use crate::vaapi_converter::VaapiConverter;

pub fn build_common_loop(
    mut app: App,
    conn: Connection,
    queue: EventQueue<App>,
    error_ping_source: PingSource,
) -> anyhow::Result<(EventLoop<'static, App>, App, LoopSignal)> {
    let event_loop: EventLoop<'static, App> =
        EventLoop::try_new().context("Error creating event loop")?;

    let loop_signal = event_loop.get_signal();
    app.loop_signal = Some(loop_signal.clone());

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registering Wayland source in event loop: {}", e))?;

    event_loop
        .handle()
        .insert_source(error_ping_source, |(), _, app| {
            if let Some(ref signal) = app.loop_signal {
                signal.stop();
            }
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder error ping: {}", e))?;

    Ok((event_loop, app, loop_signal))
}

pub fn run_with<F>(
    app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
    error_ping_source: PingSource,
    on_frame: F,
) -> anyhow::Result<()>
where
    F: Fn(&mut App) + 'static,
{
    let (mut event_loop, mut app, loop_signal) =
        build_common_loop(app, conn, queue, error_ping_source)?;

    event_loop
        .handle()
        .insert_source(ping_source, move |(), _, app| {
            on_frame(app);
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder ping: {}", e))?;

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
    Ok(())
}

pub fn run(
    app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
    error_ping_source: PingSource,
) -> anyhow::Result<()> {
    run_with(
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
        process_egl_gl,
    )
}

pub fn run_drm(
    app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
    error_ping_source: PingSource,
) -> anyhow::Result<()> {
    run_with(
        app,
        conn,
        queue,
        ping_source,
        error_ping_source,
        process_drm,
    )
}

fn process_egl_gl(app: &mut App) {
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

    let gl_ctx = app.gl_ctx.as_mut().unwrap();
    let shader: &Shader;
    if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 as u32 {
        shader = &gl_ctx.shader_yuv;
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 as u32 {
        shader = &gl_ctx.shader_nv12;
    } else {
        warn!("Unsupported pixel format, skipping frame");
        app.frame_queue.commit_read();
        return;
    }
    unsafe {
        // TODO: make the GL context explicitly current before creating/uploading
        // textures, instead of relying on the context left current by bootstrap
        if gl_ctx.textures.is_empty() {
            gl_ctx.textures = crate::render::frame::init_textures(frame_ptr);
            info!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        crate::render::frame::upload_frame(&gl_ctx.textures, frame_ptr);

        for rs in app.render_states.iter_mut() {
            crate::render::egl::eglMakeCurrent(
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

            crate::render::egl::eglSwapBuffers(rs.egl_display, rs.egl_surface);
        }
    }

    app.frame_queue.commit_read();
    app.frame_count += 1;
}

pub fn process_drm(app: &mut App) {
    let now = Instant::now();

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

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
