use ffmpeg_sys_next::*;
use tracing::{debug, info};

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
}

impl DrmFrame {
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
    ///   `*drm_frame` and must free it with `av_frame_free`; on failure it is
    ///   freed internally and set to null.
    /// - Both frames must not be accessed concurrently from other threads during
    ///   the call. The returned `DrmFrame` borrows file descriptors from
    ///   `AVDRMFrameDescriptor.objects[].fd` which remain valid while `*drm_frame`
    ///   is alive.
    /// - Caller must ensure `av_hwframe_map` and `AVDRMFrameDescriptor` layout
    ///   match the linked `ffmpeg`/`libva` version.
    pub unsafe fn map(
        frame: *mut AVFrame,
        drm_frame: &mut *mut AVFrame,
    ) -> Result<Self, anyhow::Error> {
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

            let desc = (**drm_frame).data[0] as *mut AVDRMFrameDescriptor;
            if desc.is_null() {
                av_frame_free(drm_frame);
                anyhow::bail!("AVDRMFrameDescriptor is null after map");
            }

            let mut layers = Vec::new();
            for layer_idx in 0..(*desc).nb_layers {
                let mut planes = Vec::new();
                let layer = &(*desc).layers[layer_idx as usize];

                debug!(
                    "layer[{}] format {:#x} ({}), nb_planes={}",
                    layer_idx, layer.format, layer.format, layer.nb_planes
                );

                for plane_idx in 0..layer.nb_planes {
                    let plane = &layer.planes[plane_idx as usize];
                    let obj_idx = plane.object_index as usize;

                    if obj_idx >= (*desc).nb_objects as usize {
                        av_frame_free(drm_frame);
                        anyhow::bail!("plane object_index out of range");
                    }
                    let obj = &(*desc).objects[obj_idx];

                    debug!(
                        "object[{}]: fd={}, size={}, modifier={:#x} plane[{}] offset={} pitch={}",
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

                layers.push(DrmLayer {
                    format: layer.format,
                    planes,
                });
            }

            if layers.is_empty() {
                av_frame_free(drm_frame);
                anyhow::bail!("No DRM layers found in descriptor");
            }

            let width = (*frame).width;
            let height = (*frame).height;
            let format = (*desc).layers[0].format;

            info!(
                "Mapped VAAPI frame pix_fmt={} ({}) to DRM_PRIME: {}x{} drm_fourcc={:#x} ({}) layers={}",
                (*frame).format,
                (*frame).format as u32,
                width,
                height,
                format,
                format,
                layers.len(),
            );

            let va_surface_id = (*frame).data[3] as u64;

            Ok(DrmFrame {
                layers,
                width,
                height,
                format,
                va_surface_id,
            })
        }
    }
}
