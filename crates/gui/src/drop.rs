use std::io::Read;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_backend::client::Backend;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

const URI_LIST: &str = "text/uri-list";

pub enum DropEvent {
    Hovering(bool),
    Files(Vec<String>),
}

struct State {
    connection: Connection,
    offer: Option<WlDataOffer>,
    events: Sender<DropEvent>,
    ctx: eframe::egui::Context,
}

impl State {
    fn send(&self, event: DropEvent) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

// winit has no drag and drop on wayland, so files dropped on the window are read straight from the seat's data device
pub fn listen(
    window: &impl HasDisplayHandle,
    ctx: eframe::egui::Context,
) -> Result<Receiver<DropEvent>, String> {
    let display = window.display_handle().map_err(|e| e.to_string())?;
    let RawDisplayHandle::Wayland(display) = display.as_raw() else {
        return Err("not a wayland session".to_owned());
    };
    // the display belongs to winit and lives as long as the window, which is the whole app
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| e.to_string())?;
    let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).map_err(|e| e.to_string())?;
    let device = manager.get_data_device(&seat, &qh, ());

    let (tx, rx) = mpsc::channel();
    let mut state = State {
        connection,
        offer: None,
        events: tx,
        ctx,
    };
    std::thread::Builder::new()
        .name("mux-drop".to_owned())
        .spawn(move || {
            let _keep = (seat, manager, device);
            while queue.blocking_dispatch(&mut state).is_ok() {}
        })
        .map_err(|e| e.to_string())?;
    Ok(rx)
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::Enter { serial, id, .. } => {
                let files = id.as_ref().is_some_and(|offer| {
                    offer
                        .data::<Mutex<Vec<String>>>()
                        .is_some_and(|m| m.lock().is_ok_and(|m| m.iter().any(|t| t == URI_LIST)))
                });
                match id {
                    Some(offer) if files => {
                        offer.accept(serial, Some(URI_LIST.to_owned()));
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                        state.offer = Some(offer);
                        state.send(DropEvent::Hovering(true));
                    }
                    Some(offer) => offer.destroy(),
                    None => {}
                }
            }
            // clipboard offers arrive here too, and the clipboard is left to winit
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            wl_data_device::Event::Leave => {
                if let Some(offer) = state.offer.take() {
                    offer.destroy();
                    state.send(DropEvent::Hovering(false));
                }
            }
            wl_data_device::Event::Drop => {
                let Some(offer) = state.offer.take() else {
                    return;
                };
                state.send(DropEvent::Hovering(false));
                let Ok((mut reader, writer)) = std::io::pipe() else {
                    return;
                };
                offer.receive(URI_LIST.to_owned(), std::os::fd::AsFd::as_fd(&writer));
                let _ = state.connection.flush();
                // the source writes until it closes its end, which only works once ours is closed too
                drop(writer);
                let mut list = String::new();
                let _ = reader.read_to_string(&mut list);
                offer.finish();
                offer.destroy();
                let _ = state.connection.flush();
                let files = parse_uri_list(&list);
                if !files.is_empty() {
                    state.send(DropEvent::Files(files));
                }
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mutex::new(Vec::new())),
    ]);
}

// each offer lists the formats it can be read as before it is entered or dropped
impl Dispatch<WlDataOffer, Mutex<Vec<String>>> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        types: &Mutex<Vec<String>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Ok(mut types) = types.lock()
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore WlSeat);
wayland_client::delegate_noop!(State: ignore WlDataDeviceManager);

// file managers send one file:// uri per line, with spaces and other bytes percent encoded
fn parse_uri_list(list: &str) -> Vec<String> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("file://"))
        // a host part is allowed before the path, and is always this machine for a local drop
        .filter_map(|rest| rest.find('/').map(|i| &rest[i..]))
        .map(percent_decode)
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::parse_uri_list;

    #[test]
    fn reads_paths_from_a_uri_list() {
        let list = "# from dolphin\r\nfile:///home/me/My%20Shot.png\r\nfile://host/tmp/a.txt\r\nhttps://example.com/x\r\n";
        assert_eq!(parse_uri_list(list), ["/home/me/My Shot.png", "/tmp/a.txt"]);
    }
}
