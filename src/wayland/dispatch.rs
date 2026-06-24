use tracing::{info, warn};
use wayland_client::{
    delegate_noop,
    globals::GlobalListContents,
    protocol::{
        wl_compositor::WlCompositor,
        wl_output::{self, WlOutput},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::app::state::{App, Monitor};

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        state: &mut App,
        proxy: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<App>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } if interface == "wl_output" => {
                let output = proxy.bind::<WlOutput, _, _>(name, version, qh, ());
                state.monitors.push(Monitor::new(output.clone()));
            }
            wl_registry::Event::GlobalRemove { .. } => {
                // drop WlOutput
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for App {
    fn event(
        state: &mut App,
        proxy: &WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            if let Some(monitor) = state
                .monitors
                .iter_mut()
                .find(|o| o.output.as_ref().is_some_and(|out| out.id() == proxy.id()))
            {
                monitor.physical_width = width as u32;
                monitor.physical_height = height as u32;
                info!("Output {} detected: {}x{}", proxy.id(), width, height);
            }
        }
    }
}

delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);

impl Dispatch<ZwlrLayerSurfaceV1, usize> for App {
    fn event(
        state: &mut App,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                if let Some(monitor) = state.monitors.get_mut(*data) {
                    if !state.configured {
                        monitor.logical_width = width;
                        monitor.logical_height = height;
                        monitor.configured = true;

                        if let Some(vp) = &monitor.viewport {
                            vp.set_destination(width as i32, height as i32);
                            info!("Viewport destination set: {}x{}", width, height);
                        }

                        info!(
                            "Render target: ( output: {}x{}, logical: {}x{} )",
                            monitor.physical_width,
                            monitor.physical_height,
                            monitor.logical_width,
                            monitor.logical_height
                        );

                        state.configured = state.monitors.iter().all(|m| m.configured);
                    }
                }

                proxy.ack_configure(serial);
                for monitor in state.monitors.iter() {
                    if let Some(surface) = &monitor.surface {
                        surface.commit();
                    }
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                warn!("Layer surface closed by the compositor");
                if let Some(signal) = &state.loop_signal {
                    signal.stop();
                }
            }
            _ => {}
        }
    }
}
