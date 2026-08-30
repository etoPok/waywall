use std::ffi::CString;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use calloop::ping;
use ffmpeg_sys_next::*;
use tracing::{error, info, warn};

use crate::frame_queue::FrameQueue;
use crate::notifier::Notifier;

unsafe extern "C" fn vaapi_get_format(
    ctx: *mut AVCodecContext,
    formats: *const AVPixelFormat,
) -> AVPixelFormat {
    if !(*ctx).hw_device_ctx.is_null() {
        let mut fmt_ptr = formats;
        while *fmt_ptr != AVPixelFormat::AV_PIX_FMT_NONE {
            if *fmt_ptr == AVPixelFormat::AV_PIX_FMT_VAAPI {
                if try_init_vaapi_hw(ctx) {
                    return AVPixelFormat::AV_PIX_FMT_VAAPI;
                }
                break;
            }
            fmt_ptr = fmt_ptr.add(1);
        }
    } else {
        warn!("get_format: hw_device_ctx is null. Skipping hardware.");
    }

    let mut fmt_ptr = formats;
    while *fmt_ptr != AVPixelFormat::AV_PIX_FMT_NONE {
        if *fmt_ptr == AVPixelFormat::AV_PIX_FMT_NV12
            || *fmt_ptr == AVPixelFormat::AV_PIX_FMT_YUV420P
            || *fmt_ptr == AVPixelFormat::AV_PIX_FMT_YUV420P10LE
            || *fmt_ptr == AVPixelFormat::AV_PIX_FMT_P010LE
        {
            info!("get_format: using software fallback: {:?}", *fmt_ptr);
            return *fmt_ptr;
        }
        fmt_ptr = fmt_ptr.add(1);
    }

    error!("get_format: No usable format found (neither hardware nor software)");
    AVPixelFormat::AV_PIX_FMT_NONE
}

fn try_init_vaapi_hw(ctx: *mut AVCodecContext) -> bool {
    unsafe {
        let mut hw_frames_ref: *mut AVBufferRef = ptr::null_mut();
        let ret = avcodec_get_hw_frames_parameters(
            ctx,
            (*ctx).hw_device_ctx,
            AVPixelFormat::AV_PIX_FMT_VAAPI,
            &mut hw_frames_ref,
        );

        if ret < 0 {
            warn!(
                "get_format: avcodec_get_hw_frames_parameters failed ({}). Trying manual alloc.",
                ret
            );

            if !hw_frames_ref.is_null() {
                av_buffer_unref(&mut hw_frames_ref);
            }

            hw_frames_ref = av_hwframe_ctx_alloc((*ctx).hw_device_ctx);
            if hw_frames_ref.is_null() {
                error!("get_format: av_hwframe_ctx_alloc failed.");
                return false;
            }

            let frames_ctx = (*hw_frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12; // 8-bit
            (*frames_ctx).width = (*ctx).width;
            (*frames_ctx).height = (*ctx).height;
            (*frames_ctx).initial_pool_size = 16;
        } else {
            info!("get_format: avcodec_get_hw_frames_parameters successful");
            let frames_ctx = (*hw_frames_ref).data as *mut AVHWFramesContext;
            info!("  - format: {:?}", (*frames_ctx).format);
            info!("  - sw_format: {:?}", (*frames_ctx).sw_format);
            info!("  - width: {}", (*frames_ctx).width);
            info!("  - height: {}", (*frames_ctx).height);
            info!("  - initial_pool_size: {}", (*frames_ctx).initial_pool_size);
        }

        let ret = av_hwframe_ctx_init(hw_frames_ref);
        if ret < 0 {
            error!("get_format: av_hwframe_ctx_init failed ({})", ret);
            av_buffer_unref(&mut hw_frames_ref);
            return false;
        }

        (*ctx).hw_frames_ctx = hw_frames_ref;
        true
    }
}

#[allow(dead_code)]
pub struct Decoder {
    pub thread: Option<JoinHandle<()>>,
    pub running: Arc<AtomicBool>,
    pub time_base: f64,
    pub width: i32,
    pub height: i32,
    pub pixel_format: AVPixelFormat,
    pub hw_device_ctx: *mut AVBufferRef,
}

impl Decoder {
    pub fn start(
        path: &str,
        queue: Arc<FrameQueue>,
        notifier: Notifier,
        error_ping: ping::Ping,
        use_vaapi: bool,
    ) -> Result<Self> {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let mut fmt_ctx: *mut AVFormatContext = std::ptr::null_mut();
        let path_c = std::ffi::CString::new(path)?;

        unsafe {
            let ret = avformat_open_input(
                &mut fmt_ctx,
                path_c.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if ret != 0 {
                anyhow::bail!("avformat_open_input failed: {} {}", path, ret);
            }

            let ret = avformat_find_stream_info(fmt_ctx, std::ptr::null_mut());
            if ret < 0 {
                avformat_close_input(&mut fmt_ctx);
                anyhow::bail!("avformat_find_stream_info failed");
            }
        }

        let (video_stream_idx, codec_params, time_base_num, time_base_den, width, height) =
            set_video_stream(fmt_ctx)?;

        if video_stream_idx < 0 {
            anyhow::bail!("No video stream found");
        }

        // Find decoder
        let codec_id = unsafe { (*codec_params).codec_id };
        let codec = unsafe { avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            let codec_id_int = codec_id as i32;
            unsafe { avformat_close_input(&mut fmt_ctx) };
            anyhow::bail!("No decoder found for codec_id={}", codec_id_int);
        }

        // Allocate codec context
        let mut codec_ctx = unsafe { avcodec_alloc_context3(codec) };
        if codec_ctx.is_null() {
            unsafe { avformat_close_input(&mut fmt_ctx) };
            anyhow::bail!("avcodec_alloc_context3 failed");
        }

        unsafe {
            let ret = avcodec_parameters_to_context(codec_ctx, codec_params);
            if ret < 0 {
                avcodec_free_context(&mut codec_ctx);
                avformat_close_input(&mut fmt_ctx);
                anyhow::bail!("avcodec_parameters_to_context failed");
            }
        }

        let mut hw_device_ctx = if use_vaapi {
            match init_hw_device(codec_ctx) {
                Ok(v) => v,
                Err(e) => {
                    unsafe {
                        avcodec_free_context(&mut codec_ctx);
                        avformat_close_input(&mut fmt_ctx);
                    }
                    return Err(e);
                }
            }
        } else {
            ptr::null_mut()
        };

        unsafe {
            let ret = avcodec_open2(codec_ctx, codec, std::ptr::null_mut());
            if ret < 0 {
                if use_vaapi {
                    av_buffer_unref(&mut hw_device_ctx);
                }
                avcodec_free_context(&mut codec_ctx);
                avformat_close_input(&mut fmt_ctx);
                anyhow::bail!("avcodec_open2 failed");
            }
        }

        let time_base = time_base_num as f64 / time_base_den as f64;
        let pixel_format = unsafe { (*codec_ctx).pix_fmt };

        info!(
            "Decoder: {}x{}, time_base={}/{}={}, pix_fmt={:?}",
            width, height, time_base_num, time_base_den, time_base, pixel_format as i32
        );

        let fmt_ctx_raw = fmt_ctx as usize;
        let codec_ctx_raw = codec_ctx as usize;
        let thread = thread::Builder::new()
            .name("decoder".into())
            .spawn(move || {
                let fmt_ctx = fmt_ctx_raw as *mut AVFormatContext;
                let codec_ctx = codec_ctx_raw as *mut AVCodecContext;
                decode_loop(
                    fmt_ctx,
                    codec_ctx,
                    video_stream_idx,
                    queue,
                    notifier,
                    &running_clone,
                    error_ping,
                );
            })
            .context("Failed to spawn decoder thread")?;

        Ok(Self {
            thread: Some(thread),
            running,
            time_base,
            width,
            height,
            pixel_format,
            hw_device_ctx,
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();

        if !self.hw_device_ctx.is_null() {
            unsafe { av_buffer_unref(&mut self.hw_device_ctx) };
        }
    }
}

unsafe impl Send for Decoder {}
unsafe impl Sync for Decoder {}

fn set_video_stream(
    fmt_ctx: *mut AVFormatContext,
) -> Result<(i32, *mut AVCodecParameters, i32, i32, i32, i32)> {
    unsafe {
        let nb_steams = (*fmt_ctx).nb_streams;
        for i in 0..nb_steams {
            let stream = *(*fmt_ctx).streams.offset(i as isize);
            let stream_ref = &*stream;
            if stream_ref.codecpar.as_ref().unwrap().codec_type == AVMediaType::AVMEDIA_TYPE_VIDEO {
                return Ok((
                    i as i32,
                    stream_ref.codecpar,
                    stream_ref.time_base.num,
                    stream_ref.time_base.den,
                    (*stream_ref.codecpar).width,
                    (*stream_ref.codecpar).height,
                ));
            }
        }
    }

    Ok((-1, ptr::null_mut(), 0, 0, 0, 0))
}

fn init_hw_device(codec_ctx: *mut AVCodecContext) -> Result<*mut AVBufferRef> {
    let mut hw_device_ctx: *mut AVBufferRef = ptr::null_mut();

    unsafe {
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            CString::new("/dev/dri/renderD129").unwrap().as_ptr(),
            ptr::null_mut(),
            0,
        );

        if ret < 0 {
            anyhow::bail!("av_hwdevice_ctx_create failed");
        }

        (*codec_ctx).get_format = Some(vaapi_get_format);
        (*codec_ctx).hw_device_ctx = av_buffer_ref(hw_device_ctx);
    }

    Ok(hw_device_ctx)
}

fn decode_loop(
    mut fmt_ctx: *mut AVFormatContext,
    mut codec_ctx: *mut AVCodecContext,
    video_stream_idx: i32,
    queue: Arc<FrameQueue>,
    notifier: Notifier,
    running: &AtomicBool,
    error_ping: ping::Ping,
) {
    let mut packet = unsafe { av_packet_alloc() };
    if packet.is_null() {
        error!("Failed to allocate packet");
        return;
    }

    let mut fatal = false;

    'decode: while running.load(Ordering::Relaxed) {
        let ret = unsafe { av_read_frame(fmt_ctx, packet) };

        if ret < 0 {
            if ret == AVERROR_EOF {
                // Before flushing at EOF, we must send a NULL packet to signal end-of-stream.
                // Modern codecs (H.264, HEVC) use frame reordering (B-frames) and maintain an internal
                // decoding delay. When we reach EOF, the decoder may have fully decoded frames buffered
                // internally waiting for their presentation order, or partial data that needs future packets.
                // Sending NULL tells the decoder "no more packets will arrive", allowing it to:
                // 1. Release all buffered frames in presentation order
                // 2. Discard incomplete data that can never be completed
                // Without this step, avcodec_flush_buffers would destroy the decoder state and lose
                // the final frames, causing visible stuttering or frame drops at the loop boundary.
                unsafe {
                    avcodec_send_packet(codec_ctx, std::ptr::null_mut());
                    drain_decoder(&mut codec_ctx, &queue, &notifier);
                    av_seek_frame(fmt_ctx, video_stream_idx, 0, AVSEEK_FLAG_BACKWARD);
                    avcodec_flush_buffers(codec_ctx);
                }
                info!("Decoder: EOF, restarting playback loop");
                continue;
            } else {
                error!("Error reading frame: {}", ret);
                fatal = true;
                break 'decode;
            }
        }

        let stream_idx = unsafe { (*packet).stream_index };
        if stream_idx != video_stream_idx {
            unsafe { av_packet_unref(packet) };
            continue;
        }

        let mut sent = false;
        while !sent {
            let send_ret = unsafe { avcodec_send_packet(codec_ctx, packet) };
            match send_ret {
                0 => {
                    unsafe {
                        av_packet_unref(packet);
                    }
                    drain_decoder(&mut codec_ctx, &queue, &notifier);
                    sent = true;
                }
                ret if ret == AVERROR(EAGAIN) => {
                    drain_decoder(&mut codec_ctx, &queue, &notifier);
                }
                send_ret => {
                    warn!("Error sending packet: {}", send_ret);
                    unsafe { av_packet_unref(packet) };
                    break;
                }
            }
        }
    }

    info!("Decoder thread exiting");
    unsafe {
        av_packet_free(&mut packet);
        avcodec_free_context(&mut codec_ctx);
        avformat_close_input(&mut fmt_ctx);
    }

    if fatal {
        error_ping.ping();
    }
}

fn drain_decoder(codec_ctx: &mut *mut AVCodecContext, queue: &FrameQueue, notifier: &Notifier) {
    loop {
        let slot = queue.get_write_slot();
        let recv_ret = unsafe { avcodec_receive_frame(*codec_ctx, slot) };
        if recv_ret >= 0 {
            queue.commit_write();
            notifier.0.ping();
        } else {
            break;
        }
    }
}
