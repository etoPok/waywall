use wayland_client::Proxy;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{QueueHandle, protocol::wl_compositor::WlCompositor};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::app::state::App;

pub fn create_surface(
    compositor: &WlCompositor,
    wl_output: &WlOutput,
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<App>,
) -> (WlSurface, ZwlrLayerSurfaceV1) {
    let surface = compositor.create_surface(qh, ());
    let empty_region = compositor.create_region(qh, ());
    surface.set_input_region(Some(&empty_region));
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        Some(wl_output),
        zwlr_layer_shell_v1::Layer::Background,
        "waywall".to_string(),
        qh,
        wl_output.id(),
    );

    layer_surface.set_anchor(
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Bottom
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
    );
    layer_surface.set_size(0, 0);
    layer_surface.set_margin(0, 0, 0, 0);
    layer_surface.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    layer_surface.set_exclusive_zone(-1);

    surface.commit();
    empty_region.destroy();

    (surface, layer_surface)
}
