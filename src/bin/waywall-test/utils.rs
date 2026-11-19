use std::ffi::CStr;

#[allow(dead_code)]
pub fn pix_fmt_name(fmt: i32) -> String {
    let name_ptr = unsafe {
        let pix_fmt = if (-1..=266).contains(&fmt) {
            std::mem::transmute::<i32, ffmpeg_sys_next::AVPixelFormat>(fmt)
        } else {
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE
        };
        ffmpeg_sys_next::av_get_pix_fmt_name(pix_fmt)
    };
    if name_ptr.is_null() {
        format!("unknown({fmt})")
    } else {
        unsafe { CStr::from_ptr(name_ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

