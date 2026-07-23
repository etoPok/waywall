use std::ffi::CString;
use std::ptr;

use anyhow::Result;
use ffmpeg_sys_next::*;

pub struct VaapiConverter {
    graph: *mut AVFilterGraph,
    src_ctx: *mut AVFilterContext,
    sink_ctx: *mut AVFilterContext,
}

impl VaapiConverter {
    pub fn new(
        hw_device_ctx: *mut AVBufferRef,
        width: i32,
        height: i32,
    ) -> Result<Self> {
        unsafe {
            let mut graph = avfilter_graph_alloc();
            if graph.is_null() {
                anyhow::bail!("avfilter_graph_alloc failed");
            }

            let src_filter = avfilter_get_by_name(b"buffer\0".as_ptr() as *const i8);
            if src_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('buffer') failed");
            }
            let mut src_ctx: *mut AVFilterContext = ptr::null_mut();

            let src_args = CString::new(format!(
                "video_size={}x{}:pix_fmt={}:time_base=1/30:pixel_aspect=1/1",
                width, height,
                AVPixelFormat::AV_PIX_FMT_NV12 as i32,
            ))?;

            let ret = avfilter_graph_create_filter(
                &mut src_ctx,
                src_filter,
                b"in\0".as_ptr() as *const i8,
                src_args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("create buffer src failed: {}", ret);
            }

            let mut in_hw_frames = av_hwframe_ctx_alloc(hw_device_ctx);
            let in_ctx = (*in_hw_frames).data as *mut AVHWFramesContext;
            (*in_ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*in_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*in_ctx).width = width;
            (*in_ctx).height = height;
            (*in_ctx).initial_pool_size = 4;
            av_hwframe_ctx_init(in_hw_frames);

            let params = av_buffersrc_parameters_alloc();
            (*params).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            (*params).width = width;
            (*params).height = height;
            (*params).hw_frames_ctx = av_buffer_ref(in_hw_frames);
            av_buffersrc_parameters_set(src_ctx, params);
            if !(*params).hw_frames_ctx.is_null() {
                av_buffer_unref(&mut (*params).hw_frames_ctx);
            }
            av_free(params as *mut libc::c_void);
            av_buffer_unref(&mut in_hw_frames);

            let scale_filter = avfilter_get_by_name(b"scale_vaapi\0".as_ptr() as *const i8);
            if scale_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('scale_vaapi') failed");
            }
            let mut scale_ctx: *mut AVFilterContext = ptr::null_mut();

            let scale_args = CString::new(format!("w={}:h={}:format=bgra", width, height))?;

            let ret = avfilter_graph_create_filter(
                &mut scale_ctx,
                scale_filter,
                b"scale\0".as_ptr() as *const i8,
                scale_args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("create scale_vaapi failed: {}", ret);
            }

            let sink_filter = avfilter_get_by_name(b"buffersink\0".as_ptr() as *const i8);
            if sink_filter.is_null() {
                avfilter_graph_free(&mut graph);
                anyhow::bail!("avfilter_get_by_name('buffersink') failed");
            }
            let mut sink_ctx: *mut AVFilterContext = ptr::null_mut();

            let ret = avfilter_graph_create_filter(
                &mut sink_ctx,
                sink_filter,
                b"out\0".as_ptr() as *const i8,
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
                b"pix_fmts\0".as_ptr() as *const i8,
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

    pub fn convert(&mut self, vaapi_frame: *mut AVFrame) -> Result<*mut AVFrame> {
        unsafe {
            let ret = av_buffersrc_add_frame(self.src_ctx, vaapi_frame);
            if ret < 0 {
                anyhow::bail!("buffersrc_add_frame failed: {}", ret);
            }

            let mut out_frame = av_frame_alloc();
            let ret = av_buffersink_get_frame(self.sink_ctx, out_frame);
            if ret < 0 {
                av_frame_free(&mut out_frame);
                if ret == AVERROR(EAGAIN) {
                    anyhow::bail!("converter needs more input");
                }
                anyhow::bail!("buffersink_get_frame failed: {}", ret);
            }

            Ok(out_frame)
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
