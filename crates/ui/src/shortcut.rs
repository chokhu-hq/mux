use egui::{Key, KeyboardShortcut, Modifiers};

const MAC: bool = cfg!(target_os = "macos");

// plain ctrl keys belong to the shell off macos, so app shortcuts add shift there like other terminals
pub const fn app(key: Key) -> KeyboardShortcut {
    if MAC {
        KeyboardShortcut::new(Modifiers::COMMAND, key)
    } else {
        KeyboardShortcut::new(
            Modifiers {
                shift: true,
                ..Modifiers::COMMAND
            },
            key,
        )
    }
}

pub fn keys(shortcut: &KeyboardShortcut) -> Vec<&'static str> {
    let m = shortcut.modifiers;
    let mut keys = Vec::new();
    // apple's menu order is control, option, shift, command
    if MAC {
        if m.ctrl {
            keys.push("⌃");
        }
        if m.alt {
            keys.push("⌥");
        }
        if m.shift {
            keys.push("⇧");
        }
        if m.command || m.mac_cmd {
            keys.push("⌘");
        }
    } else {
        if m.ctrl || m.command {
            keys.push("Ctrl");
        }
        if m.alt {
            keys.push("Alt");
        }
        if m.shift {
            keys.push("Shift");
        }
    }
    keys.push(key_label(shortcut.logical_key));
    keys
}

pub fn label(shortcut: &KeyboardShortcut) -> String {
    keys(shortcut).join(if MAC { "" } else { "+" })
}

fn key_label(key: Key) -> &'static str {
    match key {
        Key::Enter if MAC => "↩",
        Key::Enter => "Enter",
        Key::Backspace if MAC => "⌫",
        Key::Escape => "Esc",
        Key::Comma => ",",
        _ => key.symbol_or_name(),
    }
}

// more modifiers first because egui lets ctrl+shift+t match a ctrl+t shortcut
pub(crate) fn specificity(m: Modifiers) -> u8 {
    m.ctrl as u8 + m.alt as u8 + m.shift as u8 + (m.command || m.mac_cmd) as u8
}
