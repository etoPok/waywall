use std::time::{Duration, Instant};

use anyhow::Context;
use calloop::ping::PingSource;
use calloop::timer::Timer;
use calloop::{EventLoop, LoopSignal};
use calloop_wayland_source::WaylandSource;
use ffmpeg_sys_next::{AVFrame, AVPixelFormat};
use tracing::{debug, error, info, warn};
use wayland_client::{Connection, EventQueue};

use crate::app::state::App;
use crate::shader::Shader;
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
    F: Fn(&mut App, *mut AVFrame) + 'static,
{
    let (mut event_loop, mut app, loop_signal) =
        build_common_loop(app, conn, queue, error_ping_source)?;

    event_loop
        .handle()
        .insert_source(ping_source, move |(), _, app| {
            common_loop(app, &on_frame);
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
    debug!("Event loop started. Ctrl+C to exit.");
    unsafe { crate::runtime::signals::ctrlc_setup(loop_signal) };
    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in event loop")?;

    drop(app);
    Ok(())
}

pub fn run_egl_gl(
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
        on_egl_gl_frame,
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
        on_drm_frame,
    )
}

pub fn common_loop<F>(app: &mut App, on_frame: &F)
where
    F: Fn(&mut App, *mut AVFrame),
{
    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    let pts = unsafe { (*frame_ptr).pts };

    app.timing.start_once();
    app.timing.update(pts);
    match app.timing.get_delay(pts) {
        -1 => {
            app.frame_queue.commit_read();
            debug!("Frame dropped");
            return;
        }
        0 => {}
        delay_us => {
            debug!("Sleep thread");
            std::thread::sleep(Duration::from_micros(delay_us as u64));
        }
    }

    on_frame(app, frame_ptr);
}

fn on_egl_gl_frame(app: &mut App, frame: *mut AVFrame) {
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
        // TODO: make the GL context explicitly current before creating/uploading
        // textures, instead of relying on the context left current by bootstrap
        if gl_ctx.textures.is_empty() {
            gl_ctx.textures = crate::render::frame::init_textures(frame);
            debug!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        crate::render::frame::upload_frame(&gl_ctx.textures, frame);

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

fn on_drm_frame(app: &mut App, frame: *mut AVFrame) {
    if app.converter.is_none() {
        let w = unsafe { (*frame).width };
        let h = unsafe { (*frame).height };
        let frames = match app.decoder.hw_frames_ctx() {
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
