use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use ffmpeg_sys_next::*;
use tracing::{error, info};

use crate::frame_queue::FrameQueue;
use crate::notifier::Notifier;

#[allow(dead_code)]
pub struct Decoder {
    pub thread: Option<JoinHandle<()>>,
    pub running: Arc<AtomicBool>,
    pub time_base: f64,
    pub width: i32,
    pub height: i32,
    pub pixel_format: AVPixelFormat,
}

impl Decoder {
    pub fn start(path: &str, queue: Arc<FrameQueue>, notifier: Notifier) -> Result<Self> {
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

        let nb_streams = unsafe { (*fmt_ctx).nb_streams };

        // Find best video stream
        let mut video_stream_idx: i32 = -1;
        let mut codec_params: *mut AVCodecParameters = std::ptr::null_mut();
        let mut time_base_num: i32 = 0;
        let mut time_base_den: i32 = 0;
        let mut width: i32 = 0;
        let mut height: i32 = 0;

        unsafe {
            for i in 0..nb_streams {
                let stream = *(*fmt_ctx).streams.offset(i as isize);
                let stream_ref = &*stream;
                if stream_ref.codecpar.as_ref().unwrap().codec_type
                    == AVMediaType::AVMEDIA_TYPE_VIDEO
                {
                    video_stream_idx = i as i32;
                    codec_params = stream_ref.codecpar;
                    time_base_num = stream_ref.time_base.num;
                    time_base_den = stream_ref.time_base.den;
                    width = (*codec_params).width;
                    height = (*codec_params).height;
                    break;
                }
            }
        }

        if video_stream_idx < 0 {
            anyhow::bail!("No video stream found");
        }

        // Find decoder
        let codec_id = unsafe { (*codec_params).codec_id };
        let codec = unsafe { avcodec_find_decoder(codec_id) };
        if codec.is_null() {
            let codec_id_int = codec_id as i32;
            anyhow::bail!("No decoder found for codec_id={}", codec_id_int);
        }

        // Allocate codec context
        let mut codec_ctx = unsafe { avcodec_alloc_context3(codec) };
        if codec_ctx.is_null() {
            anyhow::bail!("avcodec_alloc_context3 failed");
        }

        unsafe {
            let ret = avcodec_parameters_to_context(codec_ctx, codec_params);
            if ret < 0 {
                avcodec_free_context(&mut codec_ctx);
                anyhow::bail!("avcodec_parameters_to_context failed");
            }
            let ret = avcodec_open2(codec_ctx, codec, std::ptr::null_mut());
            if ret < 0 {
                avcodec_free_context(&mut codec_ctx);
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
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

unsafe impl Send for Decoder {}
unsafe impl Sync for Decoder {}

fn decode_loop(
    mut fmt_ctx: *mut AVFormatContext,
    mut codec_ctx: *mut AVCodecContext,
    video_stream_idx: i32,
    queue: Arc<FrameQueue>,
    notifier: Notifier,
    running: &AtomicBool,
) {
    let mut packet = unsafe { av_packet_alloc() };
    if packet.is_null() {
        error!("Failed to allocate packet");
        return;
    }

    while running.load(Ordering::Relaxed) {
        let ret = unsafe { av_read_frame(fmt_ctx, packet) };

        if ret < 0 {
            // Before flushing at EOF, we must send a NULL packet to signal end-of-stream.
            // Modern codecs (H.264, HEVC) use frame reordering (B-frames) and maintain an internal
            // decoding delay. When we reach EOF, the decoder may have fully decoded frames buffered
            // internally waiting for their presentation order, or partial data that needs future packets.
            // Sending NULL tells the decoder "no more packets will arrive", allowing it to:
            // 1. Release all buffered frames in presentation order
            // 2. Discard incomplete data that can never be completed
            // Without this step, avcodec_flush_buffers would destroy the decoder state and lose
            // the final frames, causing visible stuttering or frame drops at the loop boundary.
            unsafe { avcodec_send_packet(codec_ctx, packet) };
            drain_decoder(&mut codec_ctx, &queue, &notifier);

            unsafe {
                av_seek_frame(fmt_ctx, video_stream_idx, 0, AVSEEK_FLAG_BACKWARD);
                avcodec_flush_buffers(codec_ctx);
            }
            info!("Decoder: EOF, restarting playback loop");
            continue;
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
                _ => {
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
