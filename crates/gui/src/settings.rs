use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Sense, UiBuilder, pos2, vec2};
use mux_core::Provider;
use mux_core::protocol::{UsageDay, UsageWindow};
use mux_ui::theme::{self, font, icon, space, text};
use mux_ui::{IconButton, Segmented, TooltipExt, icons};

use crate::prefs::{Appearance, Prefs, Transparency};

const NAV: f32 = 180.0;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Usage,
    About,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::General, Tab::Usage, Tab::About];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Usage => "Usage",
            Tab::About => "About",
        }
    }

    fn icon(self) -> char {
        match self {
            Tab::General => icons::GEAR,
            Tab::Usage => icons::CHART_BAR,
            Tab::About => icons::INFO,
        }
    }
}

pub struct Page<'a> {
    pub tab: &'a mut Tab,
    pub prefs: &'a mut Prefs,
    // transparency is only offered where the compositor can blur behind the window
    pub blur: bool,
    pub agents: [(Provider, bool); 2],
    pub usage: [(Provider, Vec<UsageWindow>); 2],
    // the last month of tokens by day, once the server has read the clis' history
    pub tokens: Option<(i64, Vec<UsageDay>)>,
}

impl Page<'_> {
    // the settings page that takes over the main area, returning true when it should close
    pub fn show(self, ui: &mut egui::Ui) -> bool {
        let p = theme::palette(ui);
        let mut close = false;
        let width = (ui.available_width() - space::XL * 2.0).min(860.0);
        let left = ui.max_rect().center().x - width / 2.0;
        let top = ui.max_rect().top() + space::XL * 1.5;

        let header = egui::Rect::from_min_size(pos2(left, top), vec2(width, 32.0));
        let mut head = ui.new_child(
            UiBuilder::new()
                .max_rect(header)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        head.label(
            egui::RichText::new("Settings")
                .font(font::semibold(text::TITLE))
                .color(p.text),
        );
        head.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = IconButton::new(icons::X, "Close settings")
                .show(ui)
                .tip_with_shortcut(
                    "Close settings",
                    KeyboardShortcut::new(Modifiers::NONE, Key::Escape),
                )
                .clicked();
        });

        let body_top = header.bottom() + space::XL;
        let nav = egui::Rect::from_min_max(
            pos2(left, body_top),
            pos2(left + NAV, ui.max_rect().bottom()),
        );
        let mut nav_ui = ui.new_child(UiBuilder::new().max_rect(nav));
        nav_ui.spacing_mut().item_spacing.y = 2.0;
        for tab in Tab::ALL {
            if nav_item(&mut nav_ui, tab, *self.tab == tab).clicked() {
                *self.tab = tab;
            }
        }

        let content = egui::Rect::from_min_max(
            pos2(nav.right() + space::XL, body_top),
            pos2(left + width, ui.max_rect().bottom()),
        );
        let mut content_ui = ui.new_child(UiBuilder::new().max_rect(content));
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("settings")
            .show(&mut content_ui, |ui| {
                ui.set_width(content.width());
                match *self.tab {
                    Tab::General => general(ui, self.prefs, self.blur, &self.agents),
                    Tab::Usage => {
                        crate::token_usage::show(ui, self.tokens.as_ref());
                        usage(ui, &self.usage);
                    }
                    Tab::About => about(ui),
                }
            });
        ui.allocate_rect(ui.max_rect(), Sense::hover());
        close
    }
}

fn nav_item(ui: &mut egui::Ui, tab: Tab, selected: bool) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(NAV, 32.0), Sense::click());
    let t = mux_ui::motion::spring_bool(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        mux_ui::motion::Spring::QUICK,
    );
    let fill = if selected {
        p.active
    } else {
        p.hover.gamma_multiply(t)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(theme::radius::LG), fill);
    let color = if selected { p.text } else { p.text_muted };
    ui.painter().text(
        pos2(rect.left() + space::SM + icon::MD / 2.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        tab.icon(),
        font::icon(icon::MD),
        color,
    );
    ui.painter().text(
        pos2(
            rect.left() + space::SM + icon::MD + space::SM,
            rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        tab.label(),
        font::medium(text::MD),
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn general(ui: &mut egui::Ui, prefs: &mut Prefs, blur: bool, agents: &[(Provider, bool)]) {
    let p = theme::palette(ui);
    section(ui, "Appearance", |ui| {
        setting(ui, "Theme", "Follow the system, or always use one", |ui| {
            Segmented::new(
                &mut prefs.appearance,
                &[
                    (Appearance::System, "System"),
                    (Appearance::Light, "Light"),
                    (Appearance::Dark, "Dark"),
                ],
            )
            .show(ui);
        });
        if blur {
            divider(ui);
            setting(
                ui,
                "Transparency",
                "How much of the desktop shows through the window",
                |ui| {
                    Segmented::new(
                        &mut prefs.transparency,
                        &[
                            (Transparency::Off, "Off"),
                            (Transparency::Subtle, "Subtle"),
                            (Transparency::Full, "Full"),
                        ],
                    )
                    .show(ui);
                },
            );
        }
        divider(ui);
        setting(
            ui,
            "Motion",
            "Reduced swaps springs and fades for instant changes",
            |ui| {
                Segmented::new(
                    &mut prefs.reduced_motion,
                    &[(false, "Full"), (true, "Reduced")],
                )
                .show(ui);
            },
        );
    });

    section(ui, "Coding agents", |ui| {
        for (i, &(provider, ready)) in agents.iter().enumerate() {
            if i > 0 {
                divider(ui);
            }
            ui.push_id(i, |ui| {
                let cli = crate::cli(provider);
                let detail = if ready {
                    format!("Runs the {cli} CLI found on your PATH")
                } else {
                    format!("Install the {cli} CLI to start {} agents", provider.label())
                };
                setting(ui, provider.label(), &detail, |ui| {
                    let (glyph, label, color) = if ready {
                        (icons::CHECK_CIRCLE, "Installed", p.success)
                    } else {
                        (icons::WARNING_CIRCLE, "Not found", p.warning)
                    };
                    ui.label(
                        egui::RichText::new(label)
                            .font(font::regular(text::MD))
                            .color(p.text_muted),
                    );
                    ui.label(
                        egui::RichText::new(glyph.to_string())
                            .font(font::icon(icon::MD))
                            .color(color),
                    );
                });
            });
        }
    });
}

fn usage(ui: &mut egui::Ui, usage: &[(Provider, Vec<UsageWindow>)]) {
    for (provider, windows) in usage {
        section(ui, &format!("{} plan limits", provider.label()), |ui| {
            if windows.is_empty() {
                let detail = match provider {
                    Provider::Claude => {
                        "Claude reports its limits as it replies, so they show after your next message"
                    }
                    Provider::Codex => "Asking Codex for its limits…",
                };
                setting(ui, "No usage reported yet", detail, |_| {});
            } else {
                mux_ui::usage_rows(ui, windows);
            }
        });
    }
}

fn about(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    section(ui, "Mux", |ui| {
        setting(ui, "Version", "The Mux app and server", |ui| {
            ui.label(
                egui::RichText::new(env!("CARGO_PKG_VERSION"))
                    .font(font::regular(text::MD))
                    .color(p.text_muted),
            );
        });
        divider(ui);
        let data = mux_core::protocol::data_dir()
            .to_string_lossy()
            .into_owned();
        let shown = mux_core::display_path(&data);
        setting(ui, "Data folder", &shown, |ui| {
            if mux_ui::Button::new("Open")
                .size(mux_ui::Size::Sm)
                .show(ui)
                .tip("Open in the file manager")
                .clicked()
            {
                mux_ui::chat::open_externally(&data);
            }
        });
    });
}

// a captioned card holding a group of settings
fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    ui.label(
        egui::RichText::new(title)
            .font(font::medium(text::SM + 1.0))
            .color(p.text_faint),
    );
    ui.add_space(space::SM);
    ui.push_id(title, |ui| {
        theme::card(ui)
            .inner_margin(egui::Margin::symmetric(space::LG as i8, 0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                add(ui);
            });
    });
    ui.add_space(space::XL);
}

// one setting, named and explained on the left with its control on the right
fn setting(ui: &mut egui::Ui, title: &str, detail: &str, control: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
    let title = ui
        .painter()
        .layout_no_wrap(title.to_owned(), font::medium(text::MD), p.text);
    let detail = ui.painter().layout_no_wrap(
        detail.to_owned(),
        font::regular(text::SM + 1.0),
        p.text_faint,
    );
    // the two lines are centred as a block, level with the control beside them
    let gap = 3.0;
    let top = rect.center().y - (title.size().y + gap + detail.size().y) / 2.0;
    let below = top + title.size().y + gap;
    ui.painter().galley(pos2(rect.left(), top), title, p.text);
    ui.painter()
        .galley(pos2(rect.left(), below), detail, p.text_faint);
    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    control(&mut controls);
}

fn divider(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, p.border));
}
