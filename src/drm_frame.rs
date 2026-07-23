use std::ptr;

use ffmpeg_sys_next::*;
use tracing::info;

pub struct DrmPlane {
    pub fd: std::os::unix::io::RawFd,
    pub offset: u32,
    pub stride: u32,
    pub modifier_lo: u32,
    pub modifier_hi: u32,
    pub modifier: u64,
}

pub struct DrmLayer {
    pub format: u32,
    pub planes: Vec<DrmPlane>,
}

pub struct DrmFrame {
    pub layers: Vec<DrmLayer>,
    pub width: i32,
    pub height: i32,
    pub format: u32,
    pub va_surface_id: u64,
    pub drm_frame: *mut AVFrame,

    rgb_frame: *mut AVFrame,
    rgb_frames_ctx: *mut AVBufferRef,
}

impl Drop for DrmFrame {
    fn drop(&mut self) {
        unsafe {
            if !self.rgb_frame.is_null() {
                av_frame_free(&mut self.rgb_frame);
            }
            if !self.rgb_frames_ctx.is_null() {
                av_buffer_unref(&mut self.rgb_frames_ctx);
            }
        }
    }
}

impl DrmFrame {
    pub fn map(frame: *mut AVFrame) -> Result<Self, anyhow::Error> {
        unsafe {
            let mut drm_frame = av_frame_alloc();
            if drm_frame.is_null() {
                anyhow::bail!("av_frame_alloc failed for DRM mapping");
            }

            (*drm_frame).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            let map_ret = av_hwframe_map(drm_frame, frame, AV_HWFRAME_MAP_READ as i32);

            if map_ret < 0 {
                av_frame_free(&mut drm_frame);
                anyhow::bail!("av_hwframe_map to DRM_PRIME failed: {}", map_ret);
            }

            let desc = (*drm_frame).data[0] as *mut AVDRMFrameDescriptor;
            if desc.is_null() {
                av_frame_free(&mut drm_frame);
                anyhow::bail!("AVDRMFrameDescriptor is null after map");
            }

            let mut layers = Vec::new();
            for layer_idx in 0..(*desc).nb_layers {
                let mut planes = Vec::new();
                let layer = &(*desc).layers[layer_idx as usize];

                info!("layer[{}] format {}", layer_idx, layer.format);

                for plane_idx in 0..layer.nb_planes {
                    let plane = &layer.planes[plane_idx as usize];
                    let obj_idx = plane.object_index as usize;
                    if obj_idx >= (*desc).nb_objects as usize {
                        av_frame_free(&mut drm_frame);
                        anyhow::bail!("plane object_index out of range");
                    }
                    let obj = &(*desc).objects[obj_idx];

                    info!(
                        "object[{}]: fd={}, size={}, modifier={:#x}",
                        obj_idx, obj.fd, obj.size, obj.format_modifier
                    );
                    planes.push(DrmPlane {
                        fd: obj.fd,
                        offset: plane.offset as u32,
                        stride: plane.pitch as u32,
                        modifier_lo: (obj.format_modifier & 0xFFFFFFFF) as u32,
                        modifier_hi: ((obj.format_modifier >> 32) & 0xFFFFFFFF) as u32,
                        modifier: obj.format_modifier,
                    });
                }

                layers.push(DrmLayer {
                    format: layer.format,
                    planes,
                });
            }

            if layers.is_empty() {
                av_frame_free(&mut drm_frame);
                anyhow::bail!("No DRM layers found in descriptor");
            }

            let width = (*frame).width;
            let height = (*frame).height;
            let format = (*desc).layers[0].format as u32;
            info!(
                "drm format {:#x} frame pix_fmt {}",
                format,
                (*frame).format as u32
            );

            info!(
                "Mapped VAAPI frame to DRM_PRIME: {}x{} format {} planes {}",
                width,
                height,
                format,
                layers.len()
            );

            let va_surface_id = (*frame).data[3] as u64;

            Ok(DrmFrame {
                layers,
                width,
                height,
                format,
                va_surface_id,
                drm_frame,
                rgb_frame: ptr::null_mut(),
                rgb_frames_ctx: ptr::null_mut(),
            })
        }
    }

    pub fn map_to_rgb(
        frame: *mut AVFrame,
        hw_device_ctx: *mut AVBufferRef,
    ) -> Result<Self, anyhow::Error> {
        unsafe {
            let mut rgb_frames_ctx = av_hwframe_ctx_alloc(hw_device_ctx);
            if rgb_frames_ctx.is_null() {
                anyhow::bail!("av_hwframe_ctx_alloc failed");
            }

            let ctx = (*rgb_frames_ctx).data as *mut AVHWFramesContext;
            (*ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*ctx).sw_format = AVPixelFormat::AV_PIX_FMT_BGRA;
            (*ctx).width = (*frame).width;
            (*ctx).height = (*frame).height;
            (*ctx).initial_pool_size = 4;

            let ret = av_hwframe_ctx_init(rgb_frames_ctx);
            if ret < 0 {
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_hwframe_ctx_init BGRA failed: {}", ret);
            }

            let mut rgb_frame = av_frame_alloc();
            if rgb_frame.is_null() {
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_frame_alloc failed");
            }
            (*rgb_frame).hw_frames_ctx = av_buffer_ref(rgb_frames_ctx);

            info!("get buffer");
            let ret = av_hwframe_get_buffer(rgb_frames_ctx, rgb_frame, 0);
            if ret < 0 {
                av_frame_free(&mut rgb_frame);
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_hwframe_get_buffer BGRA failed: {}", ret);
            }

            info!("transfer data");
            let ret = av_hwframe_transfer_data(rgb_frame, frame, 0);
            if ret < 0 {
                av_frame_free(&mut rgb_frame);
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_hwframe_transfer_data failed: {}", ret);
            }

            let mut drm_frame = av_frame_alloc();
            if drm_frame.is_null() {
                av_frame_free(&mut rgb_frame);
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_frame_alloc for DRM failed");
            }
            (*drm_frame).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;

            info!("map");
            let ret = av_hwframe_map(drm_frame, rgb_frame, AV_HWFRAME_MAP_READ as i32);
            if ret < 0 {
                av_frame_free(&mut drm_frame);
                av_frame_free(&mut rgb_frame);
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("av_hwframe_map BGRA->DRM failed: {}", ret);
            }

            let desc = (*drm_frame).data[0] as *mut AVDRMFrameDescriptor;
            if desc.is_null() {
                av_frame_free(&mut drm_frame);
                av_frame_free(&mut rgb_frame);
                av_buffer_unref(&mut rgb_frames_ctx);
                anyhow::bail!("DRM descriptor is null");
            }

            let mut layers = Vec::new();
            for layer_idx in 0..(*desc).nb_layers {
                let layer = &(*desc).layers[layer_idx as usize];
                let mut planes = Vec::new();

                for plane_idx in 0..layer.nb_planes {
                    let plane = &layer.planes[plane_idx as usize];
                    let obj = &(*desc).objects[plane.object_index as usize];

                    planes.push(DrmPlane {
                        fd: obj.fd,
                        offset: plane.offset as u32,
                        stride: plane.pitch as u32,
                        modifier_lo: (obj.format_modifier & 0xFFFFFFFF) as u32,
                        modifier_hi: ((obj.format_modifier >> 32) & 0xFFFFFFFF) as u32,
                        modifier: obj.format_modifier,
                    });
                }

                layers.push(DrmLayer {
                    format: layer.format as u32,
                    planes,
                });
            }

            let width = (*frame).width;
            let height = (*frame).height;
            let format = (*desc).layers[0].format as u32;
            let va_surface_id = (*frame).data[3] as u64;

            info!(
                "Mapped BGRA: {}x{} format={:#x} planes={} stride={}",
                width,
                height,
                format,
                layers.len(),
                layers[0].planes[0].stride
            );

            Ok(DrmFrame {
                layers,
                width,
                height,
                format,
                va_surface_id,
                drm_frame,
                rgb_frame,
                rgb_frames_ctx,
            })
        }
    }
}
