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
}

impl DrmFrame {
    pub fn map(frame: *mut AVFrame, drm_frame: &mut *mut AVFrame) -> Result<Self, anyhow::Error> {
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

                info!("layer[{}] format {}", layer_idx, layer.format);

                for plane_idx in 0..layer.nb_planes {
                    let plane = &layer.planes[plane_idx as usize];
                    let obj_idx = plane.object_index as usize;

                    if obj_idx >= (*desc).nb_objects as usize {
                        av_frame_free(drm_frame);
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
                av_frame_free(drm_frame);
                anyhow::bail!("No DRM layers found in descriptor");
            }

            let width = (*frame).width;
            let height = (*frame).height;
            let format = (*desc).layers[0].format;
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
            })
        }
    }
}
