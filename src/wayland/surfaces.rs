use tracing::info;
use wayland_client::Proxy;
use wayland_client::{protocol::wl_compositor::WlCompositor, QueueHandle};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::app::state::{App, Monitor};
use crate::wayland::raw::proxy_to_raw_ptr;

impl App {
    pub fn create_surfaces(
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        viewporter: Option<&WpViewporter>,
        qh: &QueueHandle<App>,
        monitor: &mut Monitor,
        monitor_index: usize,
    ) {
        let surface = compositor.create_surface(qh, ());
        let output = monitor.output.as_ref();

        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            output,
            zwlr_layer_shell_v1::Layer::Background,
            "mpvwall".to_string(),
            qh,
            monitor_index,
        );

        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
        );
        layer_surface.set_size(0, 0);
        layer_surface.set_margin(0, 0, 0, 0);
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        layer_surface.set_exclusive_zone(-1);

        // Get the native C pointer to wl_surface*.
        //
        // wayland_backend::ObjectId::as_ptr() returns the native *mut wl_proxy
        // through the public and stable sys backend API.
        monitor.wl_surface_ptr = proxy_to_raw_ptr(&surface);

        if let Some(vp) = &viewporter {
            let viewport = vp.get_viewport(&surface, qh, ());
            monitor.viewport = Some(viewport);
            info!("Viewport created for surface");
        }

        surface.commit();

        monitor.surface = Some(surface);
        monitor.layer_surface = Some(layer_surface);

        match &monitor.output {
            Some(out) => info!(
                "Layer surface created for monitor {}, waiting for compositor configure...",
                out.id()
            ),
            None => info!("Layer surface created for monitor, waiting for compositor configure..."),
        }
    }
}
