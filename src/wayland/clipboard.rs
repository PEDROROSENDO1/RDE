use std::io::Write;
use std::os::fd::OwnedFd;
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::GlobalList,
    protocol::wl_seat::WlSeat,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

pub struct WaylandClipboard {
    pub manager: Option<ZwlrDataControlManagerV1>,
    pub device: Option<ZwlrDataControlDeviceV1>,
    pub active_source: Option<ZwlrDataControlSourceV1>,
    pub pending_png: Option<Vec<u8>>,
}

impl WaylandClipboard {
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: Dispatch<ZwlrDataControlManagerV1, ()> + 'static,
    {
        let manager = globals
            .bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=2, ())
            .ok();
        Self {
            manager,
            device: None,
            active_source: None,
            pending_png: None,
        }
    }

    pub fn ensure_device<D>(&mut self, seat: &WlSeat, qh: &QueueHandle<D>)
    where
        D: Dispatch<ZwlrDataControlDeviceV1, ()> + 'static,
    {
        if self.device.is_some() {
            return;
        }
        if let Some(ref mgr) = self.manager {
            self.device = Some(mgr.get_data_device(seat, qh, ()));
        }
    }

    pub fn copy_png<D>(&mut self, png_bytes: Vec<u8>, qh: &QueueHandle<D>) -> bool
    where
        D: Dispatch<ZwlrDataControlSourceV1, ()> + 'static,
    {
        let (Some(mgr), Some(dev)) = (&self.manager, &self.device) else {
            return false;
        };

        if let Some(old) = self.active_source.take() {
            old.destroy();
        }

        let source = mgr.create_data_source(qh, ());
        source.offer("image/png".to_string());
        dev.set_selection(Some(&source));

        self.active_source = Some(source);
        self.pending_png = Some(png_bytes);
        true
    }

    pub fn handle_send(&self, mime_type: String, fd: OwnedFd) {
        if mime_type != "image/png" {
            return;
        }
        let Some(bytes) = self.pending_png.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(&bytes);
            let _ = file.flush();
        });
    }

    pub fn handle_cancelled(&mut self, source: &ZwlrDataControlSourceV1) {
        if self.active_source.as_ref() == Some(source) {
            self.active_source = None;
            self.pending_png = None;
        }
        source.destroy();
    }
}

pub fn dispatch_data_control_manager<D>(_proxy: &ZwlrDataControlManagerV1) {}

pub fn dispatch_data_control_device(
    event: zwlr_data_control_device_v1::Event,
) {
    if let zwlr_data_control_device_v1::Event::DataOffer { id } = event {
        id.destroy();
    }
}

pub fn dispatch_data_control_offer(_event: <ZwlrDataControlOfferV1 as wayland_client::Proxy>::Event) {}

pub fn dispatch_data_control_source(
    clipboard: &mut WaylandClipboard,
    proxy: &ZwlrDataControlSourceV1,
    event: zwlr_data_control_source_v1::Event,
    _conn: &Connection,
) {
    match event {
        zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
            clipboard.handle_send(mime_type, fd);
        }
        zwlr_data_control_source_v1::Event::Cancelled => {
            clipboard.handle_cancelled(proxy);
        }
        _ => {}
    }
}
