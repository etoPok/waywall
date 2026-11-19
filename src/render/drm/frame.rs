use anyhow::Result;
use ffmpeg_sys_next::*;
use tracing::debug;

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
    pub va_surface_id: *mut u8,
}

impl Default for DrmFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl DrmFrame {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            width: 0,
            height: 0,
            format: 0,
            va_surface_id: std::ptr::null_mut(),
        }
    }

    /// Maps a VAAPI `AVFrame` to a `DRM_PRIME` frame and extracts its descriptor.
    ///
    /// # Safety
    /// - `frame` must be a valid, non-null `*mut AVFrame` obtained from `av_frame_alloc`
    ///   (or `FrameQueue`) and must remain valid for the duration of the call. Its
    ///   `width`, `height`, `format` and `data[3]` (VAAPI `VASurfaceID`) must be
    ///   initialized and not concurrently freed.
    /// - `drm_frame` must be a valid `&mut *mut AVFrame`. It may be null (a new
    ///   frame will be allocated with `av_frame_alloc`) or point to a valid
    ///   `AVFrame`. On success the caller owns the `*mut AVFrame` stored in
    ///   `*drm_frame` and must free it with `av_frame_free`.
    /// - Both frames must not be accessed concurrently from other threads during
    ///   the call. The returned `DrmFrame` borrows file descriptors from
    ///   `AVDRMFrameDescriptor.objects[].fd` which remain valid while `*drm_frame`
    ///   is alive.
    /// - Caller must ensure `av_hwframe_map` and `AVDRMFrameDescriptor` layout
    ///   match the linked `ffmpeg`/`libva` version.
    pub unsafe fn map(&mut self, frame: *mut AVFrame, drm_frame: &mut *mut AVFrame) -> Result<()> {
        unsafe {
            if drm_frame.is_null() {
                *drm_frame = av_frame_alloc();
                if (*drm_frame).is_null() {
                    anyhow::bail!("av_frame_alloc failed for DRM mapping");
                }
            }

            (**drm_frame).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            let map_ret = av_hwframe_map(*drm_frame, frame, AV_HWFRAME_MAP_READ as i32);

            if map_ret < 0 {
                av_frame_free(drm_frame);
                anyhow::bail!("av_hwframe_map to DRM_PRIME failed: {}", map_ret);
            }

            let drm_frame_descriptor = (**drm_frame).data[0] as *mut AVDRMFrameDescriptor;
            if drm_frame_descriptor.is_null() {
                av_frame_free(drm_frame);
                anyhow::bail!("AVDRMFrameDescriptor is null after map");
            }

            self.layers.clear();
            for layer_idx in 0..(*drm_frame_descriptor).nb_layers {
                let mut planes = Vec::new();
                let layer = &(*drm_frame_descriptor).layers[layer_idx as usize];

                debug!(
                    "drm_frame_wrapper: nb_layers={}, layer[{}], layer_format={:#x} ({}), ",
                    (*drm_frame_descriptor).nb_layers,
                    layer_idx,
                    layer.format,
                    layer.format
                );

                for plane_idx in 0..layer.nb_planes {
                    let plane = &layer.planes[plane_idx as usize];
                    let obj_idx = plane.object_index as usize;

                    if obj_idx >= (*drm_frame_descriptor).nb_objects as usize {
                        av_frame_free(drm_frame);
                        anyhow::bail!("plane object_index out of range");
                    }
                    let obj = &(*drm_frame_descriptor).objects[obj_idx];

                    debug!(
                        "drm_frame_wrapper: nb_planes={}, object[{}], fd={}, size={}, modifier={:#x}, plane[{}], plane_offset={}, plane_pitch={}",
                        layer.nb_planes,
                        obj_idx,
                        obj.fd,
                        obj.size,
                        obj.format_modifier,
                        plane_idx,
                        plane.offset,
                        plane.pitch
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

                self.layers.push(DrmLayer {
                    format: layer.format,
                    planes,
                });
            }

            if self.layers.is_empty() {
                anyhow::bail!("No DRM layers found in descriptor");
            }

            self.width = (*frame).width;
            self.height = (*frame).height;
            self.format = (*drm_frame_descriptor).layers[0].format;
            self.va_surface_id = (*frame).data[3];

            Ok(())
        }
    }
}
