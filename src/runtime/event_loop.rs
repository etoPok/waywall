use std::time::Duration;

use crate::app::bootstrap::CompositorIntegration;
use crate::app::state::{App, Monitor};
use crate::shader::Shader;
use crate::vaapi_converter::VaapiConverter;
use anyhow::Context;
use calloop::ping::PingSource;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use ffmpeg_sys_next::{AVFrame, AVPixelFormat};
use tracing::{debug, error, info, warn};
use wayland_client::EventQueue;

pub fn build_common_loop(
    event_loop: &mut EventLoop<'static, App>,
    app: &mut App,
    queue: EventQueue<App>,
    compositor: Option<CompositorIntegration>,
    error_ping_source: PingSource,
) -> anyhow::Result<()> {
    app.main_loop_signal = Some(event_loop.get_signal());

    WaylandSource::new(app.wl_conn.conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("Error registering Wayland source in event loop: {}", e))?;

    event_loop
        .handle()
        .insert_source(error_ping_source, |(), _, app| {
            app.stop_main_loop();
        })
        .map_err(|e| anyhow::anyhow!("Error registering decoder error ping: {}", e))?;

    if let Some(CompositorIntegration {
        compositor,
        event_reader,
        channel,
    }) = compositor
    {
        let compositor_event_reader = std::rc::Rc::new(std::cell::RefCell::new(event_reader));
        let compositor_event_reader_for_spawn = std::rc::Rc::clone(&compositor_event_reader);
        let (tx, rx) = channel;
        event_loop
            .handle()
            .insert_source(rx, move |event, _, app| {
                if let calloop::channel::Event::Msg(event) = event {
                    compositor_event_reader.borrow().dispatch(&event, app);
                }
            })
            .map_err(|e| anyhow::anyhow!("Error registering compositor channel: {}", e))?;

        let mut tx = Some(tx);
        event_loop
            .handle()
            .insert_source(Timer::immediate(), move |_, _, _| {
                if let Some(tx) = tx.take()
                    && let Err(e) = compositor_event_reader_for_spawn
                        .borrow_mut()
                        .spawn(tx, compositor.clone())
                {
                    error!("{e:#}");
                }
                TimeoutAction::Drop
            })
            .map_err(|e| anyhow::anyhow!("Error registering reader spawn source: {}", e))?;
    }

    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(5)), |_, _, app| {
            let frames = app.frames_per_stats_sample;
            let fps = frames as f64 / 5.0;
            info!(
                "Stats: {:.1} fps, {} frames, {} dropped frames",
                fps, frames, app.dropped_frames
            );
            app.frames_per_stats_sample = 0;
            calloop::timer::TimeoutAction::ToDuration(Duration::from_secs(5))
        })
        .map_err(|e| anyhow::anyhow!("Error registering stats timer: {}", e))?;

    crate::runtime::signals::ctrlc_setup(app.main_loop_signal.as_ref().unwrap().clone())
        .context("Error setting up signal handlers")?;
    debug!("Event loop started. Ctrl+C to exit.");

    Ok(())
}

pub fn run(bootstrap_output: crate::app::bootstrap::BootstrapOutput) -> anyhow::Result<()> {
    let crate::app::bootstrap::BootstrapOutput {
        mut app,
        queue,
        mut event_loop,
        compositor,
        ping_source: _,
        error_ping_source,
    } = bootstrap_output;
    build_common_loop(
        &mut event_loop,
        &mut app,
        queue,
        compositor,
        error_ping_source,
    )?;

    insert_pacer(&event_loop.handle(), app.use_hwdec, app.occlusion.is_some())?;

    event_loop
        .run(None, &mut app, |_app| {})
        .context("Error in event loop")?;

    drop(app);
    Ok(())
}

pub fn insert_pacer(
    handle: &LoopHandle<'static, App>,
    use_hwdec: bool,
    use_occlusion: bool,
) -> anyhow::Result<()> {
    handle
        .insert_source(Timer::immediate(), move |_, _, app| {
            pacer_tick(app, &on_frame(use_hwdec, use_occlusion))
        })
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Error registering pacer_tick: {e:#}"))
}

pub fn on_frame(use_hwdec: bool, use_occlusion: bool) -> fn(&mut App, *mut AVFrame) {
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

/// Reinsert the render source when it has been removed and there is at least one output
/// that is no longer occluded.
pub fn reinsert_render_source(app: &mut App) {
    let Some(occlusion) = app.occlusion.as_ref() else {
        return;
    };

    if !occlusion.render_source_removed || app.all_outputs_occluded() {
        return;
    }

    match insert_pacer(
        &occlusion.loop_handle,
        app.use_hwdec,
        app.occlusion.is_some(),
    ) {
        Ok(()) => {
            app.occlusion
                .as_mut()
                .expect("occlusion runtime is present")
                .render_source_removed = false;
        }
        Err(e) => {
            // The flag is kept, so the source is inserted again with the next status update.
            error!("{e:#}");
        }
    }
}

/// Frame pacing loop shared by the plain and the occlusion-aware paths.
pub fn pacer_tick<F>(app: &mut App, on_frame: &F) -> TimeoutAction
where
    F: Fn(&mut App, *mut AVFrame) + 'static,
{
    loop {
        let frame = match app.frame_queue.try_get_read_slot() {
            Some(f) => f,
            None => return TimeoutAction::ToDuration(Duration::from_millis(2)),
        };
        let pts = unsafe { (*frame).pts };
        app.timing.start_once();
        app.timing.update(pts);
        match app.timing.get_delay(pts) {
            -1 => {
                app.frame_queue.commit_read();
                app.dropped_frames += 1;
                debug!("Frame dropped");
            }
            0 => {
                on_frame(app, frame);
            }
            d => return TimeoutAction::ToDuration(Duration::from_micros(d as u64)),
        }
    }
}

pub fn pacer_tick_occluded<F>(app: &mut App, on_frame: &F) -> TimeoutAction
where
    F: Fn(&mut App, *mut AVFrame) + 'static,
{
    if app.all_outputs_occluded() {
        app.timing.pause();
        if let Some(occlusion) = app.occlusion.as_mut() {
            occlusion.render_source_removed = true;
        }
        debug!("All outputs are occluded, removing the render source");
        return TimeoutAction::Drop;
    }

    // The timeline is resumed here, and not when the render source is re-inserted, so the
    // paused interval is measured up to this point: whatever the event loop dispatches in
    // between (the timer source is only dispatched in the next poll cycle) does not make
    // the video jump forward.
    // It has to be done before `update()`, otherwise the pts of the first frame after the
    // pause would be compared against the pre-pause one and could look like a seek.
    if app.timing.is_paused() {
        app.timing.resume();
    }

    pacer_tick(app, on_frame)
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
        // TODO: make the GL context explicitly current before creating/uploading
        // textures, instead of relying on the context left current by bootstrap
        if gl_ctx.textures.is_empty() {
            gl_ctx.textures = crate::render::frame::init_textures(frame);
            debug!("Textures created ({} textures)", gl_ctx.textures.len());
        }

        crate::render::frame::upload_frame(&gl_ctx.textures, frame);

        for (i, rs) in app.render_states.iter_mut().enumerate() {
            if !should_render(&app.monitors, i) {
                continue;
            }

            if !rs.wl_callback_handled {
                app.skipped_frames_unhandled_wl_callback += 1;
                continue;
            }

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
            for t in 0..num_textures {
                gl::ActiveTexture(gl::TEXTURE0 + t as u32);
                gl::BindTexture(gl::TEXTURE_2D, gl_ctx.textures[t]);
            }

            gl_ctx.quad.draw();

            rs.surface.frame(&app.wl_conn.qh, i);
            rs.wl_callback_handled = false;

            crate::render::egl::eglSwapBuffers(rs.egl_display, rs.egl_surface);
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
    if app.converter.is_none() {
        let w = unsafe { (*frame).width };
        let h = unsafe { (*frame).height };
        let frames = match app.decoder.hw_frames_ctx() {
            Some(f) => f,
            None => {
                error!("VAAPI converter: hw_frames_ctx not available");
                app.frame_queue.commit_read();
                app.stop_main_loop();
                return;
            }
        };
        match unsafe { VaapiConverter::create_nv12_to_bgra_graph_filter(frames.as_ptr(), w, h) } {
            Ok(converter) => {
                app.converter = Some(converter);
            }
            Err(e) => {
                error!("Failed to create VAAPI converter: {e:#}");
                app.frame_queue.commit_read();
                app.stop_main_loop();
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
