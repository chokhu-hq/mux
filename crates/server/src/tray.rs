use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::Event;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    // brings a window forward, or opens one when there's none
    Open,
    // closes every window and stops the server and everything running in it
    Quit,
}

// the tray thread's id, so stopping can ask its message loop to end
static THREAD: AtomicU32 = AtomicU32::new(0);

// the tray icon lives on a thread of its own, since windows only delivers its clicks to a thread pumping messages
pub fn start(events: UnboundedSender<Event>) {
    std::thread::spawn(move || {
        // GetCurrentThreadId only reads the calling thread's id
        THREAD.store(
            unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() },
            Ordering::Relaxed,
        );
        let menu = Menu::new();
        let open = MenuItem::new("Open Mux", true, None);
        let quit = MenuItem::new("Quit Mux", true, None);
        if let Err(e) = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]) {
            log::warn!("no tray menu: {e}");
            return;
        }
        let built = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Mux keeps your agents and terminals running")
            .with_icon(icon())
            .build();
        let tray = match built {
            Ok(tray) => tray,
            Err(e) => {
                log::warn!("no tray icon: {e}");
                return;
            }
        };
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let send = events.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == open_id {
                Action::Open
            } else if event.id == quit_id {
                Action::Quit
            } else {
                return;
            };
            let _ = send.send(Event::Tray(action));
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = events.send(Event::Tray(Action::Open));
            }
        }));
        pump();
        drop(tray);
    });
}

// ends the message loop so the icon is taken down, since one left behind by an exited process lingers until hovered
pub fn stop() {
    let thread = THREAD.load(Ordering::Relaxed);
    if thread == 0 {
        return;
    }
    // posting WM_QUIT to a thread id is safe even if that thread has already gone
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
            thread,
            windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT,
            0,
            0,
        );
    }
    std::thread::sleep(Duration::from_millis(100));
}

// windows' own message loop, which hands the tray icon its clicks until the process ends
fn pump() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, TranslateMessage,
    };
    // msg is plain data that GetMessageW fills in before it's read
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn icon() -> Icon {
    let image = image::load_from_memory(include_bytes!("../../../assets/icons/mux-32.png"))
        .map(|i| i.to_rgba8())
        .unwrap_or_default();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height)
        .unwrap_or_else(|_| Icon::from_rgba(vec![0; 4], 1, 1).expect("a 1x1 icon is valid"))
}
