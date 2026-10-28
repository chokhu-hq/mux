use std::io::Read;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

pub enum DropEvent {
    Hovering(bool),
    Files(Vec<String>),
}

struct State {
    connection: Connection,
    offer: Option<WlDataOffer>,
    selection: Arc<Mutex<Option<WlDataOffer>>>,
    events: Sender<DropEvent>,
    ctx: eframe::egui::Context,
}

impl State {
    fn send(&self, event: DropEvent) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

// what's on the clipboard, read straight from the compositor because the xwayland fallback egui uses can't see images
pub struct Clipboard {
    connection: Connection,
    selection: Arc<Mutex<Option<WlDataOffer>>>,
}

impl Clipboard {
    // a paste as egui events: the text and the picture, whichever the clipboard has
    pub fn paste(&self) -> Vec<eframe::egui::Event> {
        let Some(offer) = self.selection.lock().ok().and_then(|s| s.clone()) else {
            return Vec::new();
        };
        let types = offer
            .data::<Mutex<Vec<String>>>()
            .and_then(|t| t.lock().ok().map(|t| t.clone()))
            .unwrap_or_default();
        let mut events = Vec::new();
        if let Some(mime) = pick(&types, &TEXT_TYPES)
            && let Some(bytes) = self.receive(&offer, mime)
        {
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
            if !text.is_empty() {
                events.push(eframe::egui::Event::Paste(text));
            }
        }
        if let Some(mime) = pick(&types, &IMAGE_TYPES)
            && let Some(bytes) = self.receive(&offer, mime)
            && let Ok(image) = image::load_from_memory(&bytes)
        {
            let rgba = image.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let image = eframe::egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            events.push(eframe::egui::Event::PasteImage(Arc::new(image)));
        }
        events
    }

    fn receive(&self, offer: &WlDataOffer, mime: &str) -> Option<Vec<u8>> {
        let (mut reader, writer) = std::io::pipe().ok()?;
        offer.receive(mime.to_owned(), std::os::fd::AsFd::as_fd(&writer));
        self.connection.flush().ok()?;
        drop(writer);
        // read on the side with a deadline, so an app that never answers can't freeze the window
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = reader.read_to_end(&mut bytes);
            let _ = tx.send(bytes);
        });
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }
}

// the first of the wanted types that's on offer, in order of preference
fn pick<'a>(offered: &[String], wanted: &[&'a str]) -> Option<&'a str> {
    wanted
        .iter()
        .find(|w| offered.iter().any(|t| t == *w))
        .copied()
}

// winit has no drag and drop on wayland, so files dropped on the window are read straight from the seat's data device
pub fn listen(
    window: &impl HasDisplayHandle,
    ctx: eframe::egui::Context,
) -> Result<(Receiver<DropEvent>, Clipboard), String> {
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
    let selection = Arc::new(Mutex::new(None));
    let clipboard = Clipboard {
        connection: connection.clone(),
        selection: selection.clone(),
    };
    let mut state = State {
        connection,
        offer: None,
        selection,
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
    Ok((rx, clipboard))
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
            // the newest clipboard offer replaces the last, which is no longer worth anything
            wl_data_device::Event::Selection { id } => {
                if let Ok(mut selection) = state.selection.lock()
                    && let Some(old) = std::mem::replace(&mut *selection, id)
                {
                    old.destroy();
                }
            }
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
