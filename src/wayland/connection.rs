use anyhow::{Context, Ok, Result};
use wayland_client::{
    Connection, EventQueue, QueueHandle,
    globals::{GlobalList, registry_queue_init},
    protocol::{wl_compositor::WlCompositor, wl_output::WlOutput},
};
use wayland_protocols::wp::{
    linux_dmabuf::zv1::client::{
        zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    },
    viewporter::client::wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

pub struct WlConnection {
    pub conn: Connection,
    pub globals: GlobalList,
    pub qh: QueueHandle<crate::app::state::App>,

    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,
    pub viewporter: Option<WpViewporter>,

    // DMA-BUF
    pub dmabuf: Option<ZwpLinuxDmabufV1>,
    pub main_device: Option<Vec<u8>>,
}

impl WlConnection {
    pub fn new(
        conn: Connection,
        globals: GlobalList,
        qh: QueueHandle<crate::app::state::App>,
        compositor: WlCompositor,
        layer_shell: ZwlrLayerShellV1,
    ) -> Self {
        Self {
            conn,
            globals,
            qh,
            compositor,
            layer_shell,
            viewporter: None,
            dmabuf: None,
            main_device: None,
        }
    }

    pub fn connect() -> Result<(Connection, GlobalList, EventQueue<crate::app::state::App>)> {
        let conn = Connection::connect_to_env()
            .context("Could not connect to Wayland server, is WAYLAND_DISPLAY set?")?;

        let (globals, queue) = registry_queue_init::<crate::app::state::App>(&conn)
            .context("Error initializing Wayland registry")?;

        Ok((conn, globals, queue))
    }

    pub fn bind_required_protocol_extensions(
        globals: &GlobalList,
        queue: &EventQueue<crate::app::state::App>,
    ) -> Result<(WlCompositor, ZwlrLayerShellV1)> {
        let qh = queue.handle();
        let compositor = globals
            .bind(&qh, 4..=5, ())
            .context("Compositor does not support wl_compositor")?;

        let layer_shell = globals
            .bind(&qh, 1..=4, ())
            .context("Compositor does not support zwlr_layer_shell_v1")?;

        Ok((compositor, layer_shell))
    }

    pub fn bind_wp_viewporter(&mut self) -> Result<()> {
        let viewporter = self
            .globals
            .bind(&self.qh, 1..=1, ())
            .context("Compositor does not support wp_viewporter")?;
        self.viewporter = Some(viewporter);
        Ok(())
    }

    pub fn bind_zwp_linux_dmabuf_v1(&mut self) -> Result<()> {
        let dmabuf = self.globals.bind(&self.qh, 4..=4, ()).context(
            "Compositor does not support zwp_linux_dmabuf_v1 v4 (required for drm pipeline)",
        )?;
        self.dmabuf = Some(dmabuf);
        Ok(())
    }

    pub fn bind_all_available_wl_outputs(&mut self) -> Vec<WlOutput> {
        let mut outputs: Vec<WlOutput> = Vec::new();
        let registry = self.globals.registry();
        for global in self.globals.contents().clone_list() {
            if global.interface == "wl_output" {
                let output = registry.bind::<WlOutput, _, _>(
                    global.name,
                    global.version.min(4),
                    &self.qh,
                    (),
                );
                outputs.push(output);
            }
        }
        outputs
    }

    pub fn get_default_feedback(&mut self) -> Result<ZwpLinuxDmabufFeedbackV1> {
        if let Some(dmabuf) = self.dmabuf.as_ref() {
            let feedback = dmabuf.get_default_feedback(&self.qh, ());
            return Ok(feedback);
        }
        anyhow::bail!("zwp_linux_dmabuf_v1 unbound before get default feedback")
    }

    pub fn wl_display(&self) -> *mut std::ffi::c_void {
        self.conn.backend().display_ptr() as *mut std::ffi::c_void
    }
}
