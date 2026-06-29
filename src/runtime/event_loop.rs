use std::time::{Duration, Instant};

use anyhow::Context;
use calloop::ping::PingSource;
use calloop::timer::Timer;
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use ffmpeg_sys_next::AVPixelFormat;
use tracing::{info, warn};
use wayland_client::{Connection, EventQueue};

use crate::app::state::App;
use crate::shader::Shader;
use crate::timing::Timing;

pub fn run(
    mut app: App,
    conn: Connection,
    queue: EventQueue<App>,
    ping_source: PingSource,
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

    // Stats timer (every 5 seconds)
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

fn process_frame(app: &mut App) {
    let now = Instant::now();

    let frame_ptr_opt = app.frame_queue.try_get_read_slot();
    let frame_ptr = match frame_ptr_opt {
        Some(ptr) => ptr,
        None => return,
    };

    let pts = unsafe { (*frame_ptr).pts };

    // Resume timing if first frame
    if app.timing.is_none() {
        if let Some(ref decoder) = app.decoder {
            app.timing = Some(Timing::new(decoder.time_base));
            info!("Timing initialized: time_base={}", decoder.time_base);
        }
    }

    // Detect seek: non-monotonic PTS jump backwards → reset timing
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
