use anyhow::Result;
use libmpv2::Mpv;
use tracing::info;

pub fn init_mpv() -> Result<Mpv> {
    let mpv = Mpv::with_initializer(|init| {
        init.set_property("terminal", "no")?;
        init.set_property("msg-level", "all=warn,vd=info")?;
        init.set_property("loop-file", "inf")?;
        init.set_property("loop", "inf")?;
        init.set_property("mute", true)?;
        init.set_property("audio", false)?;
        init.set_property("osc", false)?;
        init.set_property("osd-level", 0_i64)?;
        init.set_property("pause", false)?;
        init.set_property("hwdec", "auto-safe")?;
        init.set_property("keepaspect", false)?;
        init.set_property("input-default-bindings", false)?;
        init.set_property("input-vo-keyboard", false)?;
        init.set_property("input-cursor", false)?;
        init.set_property("vo", "libmpv")?;
        // Render API (vo=libmpv) only supports OpenGL.
        init.set_property("gpu-api", "opengl")?;
        // NOTE: gpu-context is NOT set here; with vo=libmpv the context is provided
        // by the application via mpv_render_context_create.
        // display-resample requires compositor timing that vo=libmpv doesn't provide
        init.set_property("video-sync", "audio")?;
        // framedrop=vo is UB with vo=libmpv
        init.set_property("framedrop", "no")?;
        Ok(())
    })
    .map_err(|e| anyhow::anyhow!("Error inicializando libmpv: {}", e))?;

    info!("libmpv initialized (gpu-api=opengl, render API mode, no window)");
    Ok(mpv)
}
