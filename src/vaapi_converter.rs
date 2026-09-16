use std::ffi::CString;
use std::ptr;

use anyhow::Result;
use ffmpeg_sys_next::*;
use tracing::debug;

pub struct VaapiConverter {
    graph: *mut AVFilterGraph,
    src_ctx: *mut AVFilterContext,
    sink_ctx: *mut AVFilterContext,
}

impl VaapiConverter {
    /// Creates a VAAPI `scale_vaapi` filter graph for `NV12 -> BGRA` conversion.
    ///
    /// # Safety
    /// - `hw_device_ctx` must be a valid, non-null `*mut AVBufferRef` obtained
    ///   from `av_hwdevice_ctx_create` with `AV_HWDEVICE_TYPE_VAAPI` (e.g.
    ///   `Decoder.hw_device_ctx`) and must remain valid for the duration of the
    ///   call. The graph retains a reference via `av_buffer_ref`.
    /// - `width` and `height` must be positive and match the dimensions of
    ///   frames that will be fed to `convert` (VAAPI `AVFrame.width/height`).
    ///   (caller must ensure the decoder was configured with compatible
    ///   `AVHWFramesContext`).
    /// - Caller must ensure FFmpeg `libavfilter` is initialized for `VAAPI`
    ///   and that `scale_vaapi`/`buffer`/`buffersink` filters are available.
    pub unsafe fn new(hw_frames_ctx: *mut AVBufferRef, width: i32, height: i32) -> Result<Self> {
        unsafe {
            let mut graph = avfilter_graph_alloc();
            if graph.is_null() {
                anyhow::bail!("avfilter_graph_alloc failed");
            }

            let src_filter = avfilter_get_by_name(c"buffer".as_ptr());
            if src_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('buffer') failed");
            }
            let mut src_ctx: *mut AVFilterContext = ptr::null_mut();

            let src_args = CString::new(format!(
                "video_size={}x{}:pix_fmt={}:time_base=1/30:pixel_aspect=1/1:colorspace=bt709:range=tv",
                width,
                height,
                AVPixelFormat::AV_PIX_FMT_NV12 as i32,
            ))?;

            let ret = avfilter_graph_create_filter(
                &mut src_ctx,
                src_filter,
                c"in".as_ptr(),
                src_args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("create buffer src failed: {}", ret);
            }

            let params = av_buffersrc_parameters_alloc();
            (*params).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            (*params).width = width;
            (*params).height = height;
            (*params).hw_frames_ctx = av_buffer_ref(hw_frames_ctx);
            av_buffersrc_parameters_set(src_ctx, params);
            if !(*params).hw_frames_ctx.is_null() {
                av_buffer_unref(&mut (*params).hw_frames_ctx);
            }
            av_free(params as *mut libc::c_void);

            let scale_filter = avfilter_get_by_name(c"scale_vaapi".as_ptr());
            if scale_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('scale_vaapi') failed");
            }
            let mut scale_ctx: *mut AVFilterContext = ptr::null_mut();

            let scale_args = CString::new(format!("w={}:h={}:format=bgra", width, height))?;

            let ret = avfilter_graph_create_filter(
                &mut scale_ctx,
                scale_filter,
                c"scale".as_ptr(),
                scale_args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("create scale_vaapi failed: {}", ret);
            }

            let sink_filter = avfilter_get_by_name(c"buffersink".as_ptr());
            if sink_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('buffersink') failed");
            }
            let mut sink_ctx: *mut AVFilterContext = ptr::null_mut();

            let ret = avfilter_graph_create_filter(
                &mut sink_ctx,
                sink_filter,
                c"out".as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("create buffersink failed: {}", ret);
            }

            let pix_fmts = [
                AVPixelFormat::AV_PIX_FMT_VAAPI as i32,
                AVPixelFormat::AV_PIX_FMT_NONE as i32,
            ];
            av_opt_set_bin(
                sink_ctx as *mut libc::c_void,
                c"pix_fmts".as_ptr(),
                pix_fmts.as_ptr() as *const u8,
                (pix_fmts.len() * std::mem::size_of::<i32>()) as i32,
                0,
            );

            let ret = avfilter_link(src_ctx, 0, scale_ctx, 0);
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("link src->scale failed: {}", ret);
            }

            let ret = avfilter_link(scale_ctx, 0, sink_ctx, 0);
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("link scale->sink failed: {}", ret);
            }

            let ret = avfilter_graph_config(graph, ptr::null_mut());
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("graph config failed: {}", ret);
            }

            Ok(VaapiConverter {
                graph,
                src_ctx,
                sink_ctx,
            })
        }
    }

    /// Converts a VAAPI frame to BGRA via the filter graph.
    ///
    /// # Safety
    /// - `vaapi_frame` must be a valid, non-null `*mut AVFrame` with
    ///   `format == AV_PIX_FMT_VAAPI`, obtained from the decoder queue, and
    ///   must remain valid for the call. Its `width/height/format/pts` are
    ///   read.
    /// - `bgra_frame` must be a valid `&mut *mut AVFrame`. It may be null
    ///   (a new frame will be allocated with `av_frame_alloc`) or point to a
    ///   valid `AVFrame`. On success `*bgra_frame` is owned by the caller and
    ///   must be freed with `av_frame_free`/`av_frame_unref`; on failure it
    ///   is freed internally.
    /// - `self` must have been created by `VaapiConverter::new` with matching
    ///   `width/height` and must not be concurrently accessed. The underlying
    ///   `AVFilterGraph` must remain valid.
    pub unsafe fn convert(
        &mut self,
        vaapi_frame: *mut AVFrame,
        bgra_frame: &mut *mut AVFrame,
    ) -> Result<()> {
        unsafe {
            if bgra_frame.is_null() {
                *bgra_frame = av_frame_alloc();
                if (*bgra_frame).is_null() {
                    anyhow::bail!("av_frame_alloc failed for convert!");
                }
            }

            debug!(
                "VaapiConverter::convert incoming w={} h={} fmt={} pts={} pkt_dts={} best_effort={}",
                (*vaapi_frame).width,
                (*vaapi_frame).height,
                (*vaapi_frame).format,
                (*vaapi_frame).pts,
                (*vaapi_frame).pkt_dts,
                (*vaapi_frame).best_effort_timestamp
            );

            let ret = av_buffersrc_add_frame_flags(
                self.src_ctx,
                vaapi_frame,
                AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            );

            if ret < 0 {
                anyhow::bail!("buffersrc_add_frame failed: {}", ret);
            }

            let ret = av_buffersink_get_frame(self.sink_ctx, *bgra_frame);
            if ret < 0 {
                av_frame_free(bgra_frame);
                if ret == AVERROR(EAGAIN) {
                    anyhow::bail!("converter needs more input");
                }
                anyhow::bail!("buffersink_get_frame failed: {}", ret);
            }

            Ok(())
        }
    }
}

impl Drop for VaapiConverter {
    fn drop(&mut self) {
        unsafe {
            if !self.graph.is_null() {
                avfilter_graph_free(&mut self.graph);
            }
        }
    }
}
