use tracing::{debug, error, warn};
use wayland_backend::client::ObjectId;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
    globals::GlobalListContents,
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_output::{self, WlOutput},
        wl_region::WlRegion,
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

use wayland_protocols::wp::{
    linux_dmabuf::zv1::client::{
        zwp_linux_buffer_params_v1, zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::app::state::App;

delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_noop!(App: ignore ZwpLinuxDmabufV1);
delegate_noop!(App: ignore WlRegion);

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _state: &mut App,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
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
        if let Some(monitor) = state
            .monitors
            .iter_mut()
            .find(|o| o.proxy.id() == proxy.id())
        {
            match event {
                wl_output::Event::Mode { width, height, .. } => {
                    monitor.physical_width = width as u32;
                    monitor.physical_height = height as u32;
                    debug!("Output {} detected: {}x{}", proxy.id(), width, height);
                }
                wl_output::Event::Name { name } => {
                    debug!("Output {} detected", name);
                    monitor.name = Some(name);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ObjectId> for App {
    fn event(
        state: &mut App,
        proxy: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        data: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        let output_idx = state.monitors.iter().position(|o| o.proxy.id() == *data);
        if let Some(index) = output_idx {
            match event {
                zwlr_layer_surface_v1::Event::Configure {
                    serial,
                    width,
                    height,
                } => {
                    proxy.ack_configure(serial);

                    let monitor = if let Some(m) = state.monitors.get_mut(index) {
                        m
                    } else {
                        return;
                    };

                    monitor.layer_surface_configure = Some((width, height));
                    if let Some(vp) = &monitor.viewport {
                        vp.set_destination(width as i32, height as i32);

                        debug!(
                            "Render target: ( output: {}x{}, logical: {}x{} )",
                            monitor.physical_width, monitor.physical_height, width, height
                        );
                    }

                    if let Some(surface) = &monitor.surface {
                        surface.commit();
                    }
                }
                zwlr_layer_surface_v1::Event::Closed => {
                    warn!("Layer surface closed by the compositor");
                    if let Some(m) = state.monitors.get(index) {
                        state.render_states.remove(&m.proxy.id());
                    }
                    state.monitors.remove(index);
                    if state.monitors.is_empty()
                        && let Some(signal) = &state.main_loop_signal
                    {
                        signal.stop();
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for App {
    fn event(
        state: &mut App,
        proxy: &WlBuffer,
        event: wl_buffer::Event,
        _date: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        if let wl_buffer::Event::Release = event {
            for wbs in state.wl_buffer_states.iter_mut().flatten() {
                if wbs.wl_buffer.as_ref().is_some_and(|b| b.id() == proxy.id()) {
                    wbs.in_use = false;
                    break;
                }
            }
        }
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_linux_buffer_params_v1::Event::Created { buffer } => {
                debug!(
                    "zwp_linux_buffer_params_v1 created successfully: {:?}",
                    buffer.id()
                );
            }
            zwp_linux_buffer_params_v1::Event::Failed => {
                error!("zwp_linux_buffer_params_v1 failed to create");
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for App {
    fn event(
        state: &mut Self,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        event: <ZwpLinuxDmabufFeedbackV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event::MainDevice {
            device,
        } = event
        {
            state.wl_conn.main_device = Some(device);
        }
    }
}

impl Dispatch<WlCallback, ObjectId> for App {
    fn event(
        state: &mut Self,
        _proxy: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        data: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let Some(rs) = state.render_states.get_mut(data) {
            rs.wl_callback_handled = true;
        }
    }
}
