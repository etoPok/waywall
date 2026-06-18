use tracing::info;
use wayland_client::QueueHandle;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1,
    zwlr_layer_surface_v1,
};

use crate::app::state::App;
use crate::wayland::raw::proxy_to_raw_ptr;

impl App {
    pub fn create_surfaces(&mut self, qh: &QueueHandle<App>) {
        let surface = self.compositor.create_surface(qh, ());
        let output = self.output.as_ref();

        let layer_surface = self.layer_shell.get_layer_surface(
            &surface,
            output,
            zwlr_layer_shell_v1::Layer::Background,
            "mpv-wallpaper".to_string(),
            qh,
            (),
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
        self.wl_surface_ptr = proxy_to_raw_ptr(&surface);

        surface.commit();

        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);

        info!("Layer surface created, waiting for compositor configure...");
    }
}
