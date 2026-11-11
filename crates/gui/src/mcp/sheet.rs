use eframe::egui::{self, CornerRadius, RichText, Sense, vec2};
use mux_core::protocol::McpServer;
use mux_ui::theme::{self, font, radius, space, text};
use mux_ui::{Button, IconButton, Segmented, Size, TextInput, TooltipExt, icons};

use super::command::imported;
use super::{Draft, Kind};

pub(super) const SHEET: f32 = 520.0;

pub(super) const PAD: i8 = 20;

// the add, edit and import form in a sheet over the page, with a header and footer like a dialog
pub(super) fn sheet(ui: &mut egui::Ui, draft: &mut Draft, servers: &mut Vec<McpServer>) {
    let p = theme::palette(ui);
    let editing = draft
        .editing
        .as_ref()
        .and_then(|name| servers.iter().position(|s| &s.name == name));
    let (title, subtitle) = match editing {
        Some(i) => (
            format!("Edit {}", servers[i].name),
            "Agents get the change with their next message",
        ),
        None if draft.json => (
            "Paste JSON".to_owned(),
            "The \"mcpServers\" block most servers show in their README",
        ),
        None => (
            "Add an MCP server".to_owned(),
            "Every agent gets it with their next message",
        ),
    };
    let frame = theme::floating(ui).inner_margin(0);
    let modal = egui::Modal::new(ui.scope_id().with("mcp-sheet"))
        .frame(frame)
        .show(ui.ctx(), |ui| {
            ui.set_width(SHEET);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: PAD,
                    right: PAD - 6,
                    top: PAD - 2,
                    bottom: 16,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(&title)
                                    .font(font::semibold(text::MD + 2.0))
                                    .color(p.text),
                            );
                            ui.add_space(space::XS);
                            ui.label(
                                RichText::new(subtitle)
                                    .font(font::regular(text::SM + 1.0))
                                    .color(p.text_faint),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if IconButton::new(icons::X, "Close").show(ui).clicked() {
                                draft.open = false;
                            }
                        });
                    });
                });
            hairline(ui);
            egui::Frame::new()
                .inner_margin(egui::Margin::same(PAD))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if draft.json && editing.is_none() {
                        TextInput::new(&mut draft.paste)
                            .placeholder("{ \"mcpServers\": { \"name\": { \"command\": \"npx\", \"args\": [ … ] } } }")
                            .multiline(8)
                            .show(ui);
                    } else {
                        form(ui, draft);
                    }
                });
            hairline(ui);
            egui::Frame::new()
                .fill(theme::tint(p, theme::fill::CARD))
                .corner_radius(CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: radius::LG,
                    se: radius::LG,
                })
                .inner_margin(egui::Margin::symmetric(PAD, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        error(ui, &draft.error);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = match editing {
                                Some(_) => "Save",
                                None if draft.json => "Import",
                                None => "Add server",
                            };
                            if Button::new(label).primary().show(ui).clicked() {
                                save(draft, servers, editing);
                            }
                            if Button::new("Cancel").show(ui).clicked() {
                                draft.open = false;
                            }
                        });
                    });
                });
        });
    if modal.should_close() {
        draft.open = false;
    }
}

pub(super) fn hairline(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, p.border),
    );
}

// adds, saves or imports what the sheet holds, closing it once that worked
pub(super) fn save(draft: &mut Draft, servers: &mut Vec<McpServer>, editing: Option<usize>) {
    if draft.json && editing.is_none() {
        match imported(&draft.paste) {
            Ok(found) => {
                for server in found {
                    match servers.iter_mut().find(|s| s.name == server.name) {
                        Some(existing) => *existing = server,
                        None => servers.push(server),
                    }
                }
                *draft = Draft::default();
            }
            Err(e) => draft.error = e,
        }
        return;
    }
    let enabled = editing.is_none_or(|i| servers[i].enabled);
    match draft.server(enabled) {
        Ok(server) => {
            let clash = servers
                .iter()
                .enumerate()
                .any(|(i, s)| s.name == server.name && Some(i) != editing);
            if clash {
                draft.error = format!("There's already a server called {}.", server.name);
            } else {
                match editing {
                    Some(i) => servers[i] = server,
                    None => servers.push(server),
                }
                *draft = Draft::default();
            }
        }
        Err(error) => draft.error = error,
    }
}

pub(super) fn form(ui: &mut egui::Ui, draft: &mut Draft) {
    field(ui, "Name", |ui| {
        TextInput::new(&mut draft.name)
            .placeholder("my-server")
            .show(ui);
    });
    field(ui, "Runs as", |ui| {
        Segmented::new(
            &mut draft.kind,
            &[(Kind::Command, "Command"), (Kind::Url, "URL")],
        )
        .show(ui);
    });
    let reveal = &mut draft.reveal;
    match draft.kind {
        Kind::Command => {
            field(ui, "Command", |ui| {
                TextInput::new(&mut draft.command)
                    .placeholder("npx -y some-mcp-server")
                    .show(ui);
            });
            values(ui, "Environment", &mut draft.env, "API_KEY", reveal);
        }
        Kind::Url => {
            field(ui, "URL", |ui| {
                TextInput::new(&mut draft.url)
                    .placeholder("https://example.com/mcp")
                    .show(ui);
            });
            values(ui, "Headers", &mut draft.headers, "Authorization", reveal);
        }
    }
}

// name and value rows, the values dotted out until shown, since they're usually keys and tokens
pub(super) fn values(
    ui: &mut egui::Ui,
    label: &str,
    rows: &mut Vec<(String, String)>,
    placeholder: &str,
    reveal: &mut bool,
) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .font(font::medium(text::MD))
                .color(p.text),
        );
        if !rows.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, tip) = if *reveal {
                    (icons::EYE_SLASH, "Hide values")
                } else {
                    (icons::EYE, "Show values")
                };
                if IconButton::new(glyph, "Show values")
                    .size(Size::Sm)
                    .show(ui)
                    .tip(tip)
                    .clicked()
                {
                    *reveal = !*reveal;
                }
            });
        }
    });
    ui.add_space(space::XS);
    let mut removed = None;
    for (i, (key, value)) in rows.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let width = ui.available_width() - control_width() - gap * 2.0;
                TextInput::new(key)
                    .placeholder(placeholder)
                    .width(width * 0.4)
                    .show(ui);
                TextInput::new(value)
                    .placeholder("value")
                    .password(!*reveal)
                    .width(width * 0.6)
                    .show(ui);
                if IconButton::new(icons::X, "Remove")
                    .show(ui)
                    .tip("Remove")
                    .clicked()
                {
                    removed = Some(i);
                }
            });
        });
        ui.add_space(space::XS);
    }
    if let Some(i) = removed {
        rows.remove(i);
    }
    let add = if label == "Headers" {
        "Add header"
    } else {
        "Add variable"
    };
    if Button::new(format!("+ {add}"))
        .size(Size::Sm)
        .ghost()
        .show(ui)
        .clicked()
    {
        rows.push((String::new(), String::new()));
        *reveal = true;
    }
    ui.add_space(space::MD);
}

pub(super) fn control_width() -> f32 {
    mux_ui::theme::control::SM
}

pub(super) fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    ui.label(
        RichText::new(label)
            .font(font::medium(text::MD))
            .color(p.text),
    );
    ui.add_space(space::SM);
    add(ui);
    ui.add_space(space::LG);
}

pub(super) fn error(ui: &mut egui::Ui, message: &str) {
    if !message.is_empty() {
        let p = theme::palette(ui);
        ui.add(
            egui::Label::new(
                RichText::new(message)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.danger),
            )
            .wrap(),
        );
    }
}
