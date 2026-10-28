use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Sense, UiBuilder, pos2, vec2};
use mux_core::protocol::{UsageDay, UsageWindow};
use mux_core::{Access, Provider};
use mux_ui::theme::{self, font, icon, space, text};
use mux_ui::{IconButton, Segmented, TooltipExt, icons};

use crate::prefs::{Appearance, Prefs, Transparency};

const NAV: f32 = 180.0;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Usage,
    Logs,
    About,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Usage, Tab::Logs, Tab::About];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Usage => "Usage",
            Tab::Logs => "Logs",
            Tab::About => "About",
        }
    }

    fn icon(self) -> char {
        match self {
            Tab::General => icons::GEAR,
            Tab::Usage => icons::CHART_BAR,
            Tab::Logs => icons::SCROLL,
            Tab::About => icons::INFO,
        }
    }
}

pub struct Page<'a> {
    pub tab: &'a mut Tab,
    pub prefs: &'a mut Prefs,
    // transparency is only offered where the compositor can blur behind the window
    pub blur: bool,
    pub agents: [(Provider, bool); 3],
    pub usage: [(Provider, Vec<UsageWindow>); 3],
    // the last month of tokens by day, once the server has read the clis' history
    pub tokens: Option<(i64, Vec<UsageDay>)>,
    // on windows, the wsl distributions the server could run in instead
    pub distros: Option<Vec<String>>,
    pub models: crate::sessions::Models,
    pub themes: &'a mut crate::themes::Themes,
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
                    Tab::General => general(
                        ui,
                        self.prefs,
                        self.blur,
                        &self.agents,
                        self.distros.as_deref(),
                        &self.models,
                        self.themes,
                    ),
                    Tab::Usage => {
                        crate::token_usage::show(ui, self.tokens.as_ref());
                        usage(ui, &self.usage);
                    }
                    Tab::Logs => crate::logs::show(ui, &self.agents),
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

fn general(
    ui: &mut egui::Ui,
    prefs: &mut Prefs,
    blur: bool,
    agents: &[(Provider, bool)],
    distros: Option<&[String]>,
    models: &crate::sessions::Models,
    themes: &mut crate::themes::Themes,
) {
    let p = theme::palette(ui);
    if let Some(distros) = distros {
        server(ui, prefs, distros);
    }
    new_agents(ui, prefs, models);
    section(ui, "Appearance", |ui| {
        setting(ui, "Mode", "Follow the system, or always use one", |ui| {
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
        theme_rows(ui, prefs, themes);
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
        divider(ui);
        setting(
            ui,
            "Terminal text size",
            "The zoom keys in a terminal change it too",
            |ui| {
                let size = prefs.terminal_size();
                let mut next = size;
                if IconButton::new(icons::PLUS, "Larger").show(ui).clicked() {
                    next += 1.0;
                }
                ui.label(
                    egui::RichText::new(format!("{size}"))
                        .font(font::regular(text::MD))
                        .color(p.text_muted),
                );
                if IconButton::new(icons::MINUS, "Smaller").show(ui).clicked() {
                    next -= 1.0;
                }
                let next = next.clamp(mux_ui::TERMINAL_MIN_SIZE, mux_ui::TERMINAL_MAX_SIZE);
                if next != size {
                    let default = (next - text::MONO).abs() < 0.01;
                    prefs.terminal_text = (!default).then_some(next);
                }
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
    // opencode has no plan of its own to report limits for
    for (provider, windows) in usage.iter().filter(|(p, _)| *p != Provider::OpenCode) {
        section(ui, &format!("{} plan limits", provider.label()), |ui| {
            if windows.is_empty() {
                let detail = match provider {
                    Provider::Claude => {
                        "Claude reports its limits as it replies, so they show after your next message"
                    }
                    Provider::Codex | Provider::OpenCode => "Asking Codex for its limits…",
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

// where the server and its agents run on windows: beside the app, or inside a wsl distribution
fn server(ui: &mut egui::Ui, prefs: &mut Prefs, distros: &[String]) {
    section(ui, "Server", |ui| {
        let detail = if distros.is_empty() {
            "Install WSL to run agents on Linux instead"
        } else {
            "WSL runs agents and their tools on Linux"
        };
        setting(ui, "Run agents in", detail, |ui| {
            let current = prefs
                .wsl
                .as_deref()
                .map_or("Windows".to_owned(), |d| format!("WSL: {d}"));
            let button = mux_ui::Button::new(current).size(mux_ui::Size::Sm).show(ui);
            egui::Popup::menu(&button)
                .align(egui::RectAlign::BOTTOM_END)
                .gap(space::XS)
                .show(|ui| {
                    ui.set_width(220.0);
                    if mux_ui::MenuItem::new("Windows")
                        .checked(prefs.wsl.is_none())
                        .show(ui)
                        .clicked()
                    {
                        prefs.wsl = None;
                    }
                    for distro in distros {
                        let label = format!("WSL: {distro}");
                        let chosen = prefs.wsl.as_deref() == Some(distro.as_str());
                        if mux_ui::MenuItem::new(&label)
                            .checked(chosen)
                            .show(ui)
                            .clicked()
                        {
                            prefs.wsl = Some(distro.clone());
                        }
                    }
                });
        });
    });
}

// what a new agent starts with, so the composer is already set the way someone usually works
fn theme_rows(ui: &mut egui::Ui, prefs: &mut Prefs, themes: &mut crate::themes::Themes) {
    for (dark, title, detail) in [
        (true, "Dark theme", "Used while the app is dark"),
        (false, "Light theme", "Used while the app is light"),
    ] {
        let hidden = if dark {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        if prefs.appearance == hidden {
            continue;
        }
        let picked = if dark {
            &mut prefs.dark_theme
        } else {
            &mut prefs.light_theme
        };
        let fallback = if dark {
            crate::themes::MUX_DARK
        } else {
            crate::themes::MUX_LIGHT
        };
        divider(ui);
        ui.push_id(title, |ui| {
            setting(ui, title, detail, |ui| {
                let current = picked
                    .as_deref()
                    .and_then(|id| themes.find(id))
                    .filter(|c| c.dark == dark)
                    .or_else(|| themes.find(fallback));
                let label = current.map_or("Mux", |c| c.name.as_str());
                let button = mux_ui::Button::new(label).size(mux_ui::Size::Sm).show(ui);
                let current = current.map(|c| c.id.clone());
                egui::Popup::menu(&button)
                    .align(egui::RectAlign::BOTTOM_END)
                    .gap(space::XS)
                    .show(|ui| {
                        ui.set_width(220.0);
                        for choice in themes.list.iter().filter(|c| c.dark == dark) {
                            let mut item = mux_ui::MenuItem::new(&choice.name)
                                .checked(current.as_deref() == Some(choice.id.as_str()));
                            if choice.custom {
                                item = item.detail("Custom");
                            }
                            if item.show(ui).clicked() {
                                *picked = (choice.id != fallback).then(|| choice.id.clone());
                            }
                        }
                    });
            });
        });
    }
    divider(ui);
    let detail = match themes.errors.first() {
        Some((file, error)) => format!("Couldn't read {file}: {error}"),
        None => "JSON files in the themes folder, reloaded as you save them".to_owned(),
    };
    setting(ui, "Custom themes", &detail, |ui| {
        if mux_ui::Button::new("Open folder")
            .size(mux_ui::Size::Sm)
            .show(ui)
            .clicked()
        {
            let dir = crate::themes::dir();
            let _ = std::fs::create_dir_all(&dir);
            mux_ui::chat::open_externally(&dir.to_string_lossy());
        }
        let dark = ui.visuals().dark_mode;
        let shown = if dark {
            prefs
                .dark_theme
                .as_deref()
                .unwrap_or(crate::themes::MUX_DARK)
        } else {
            prefs
                .light_theme
                .as_deref()
                .unwrap_or(crate::themes::MUX_LIGHT)
        }
        .to_owned();
        let new = mux_ui::Button::new("New theme")
            .size(mux_ui::Size::Sm)
            .show(ui)
            .tip("Copy the current theme into a file to edit");
        if new.clicked() {
            match themes.duplicate(&shown) {
                Ok(id) => {
                    let file = crate::themes::dir().join(format!("{id}.json"));
                    mux_ui::chat::open_externally(&file.to_string_lossy());
                    if dark {
                        prefs.dark_theme = Some(id);
                    } else {
                        prefs.light_theme = Some(id);
                    }
                }
                Err(e) => log::warn!("couldn't create a theme: {e}"),
            }
        }
    });
}

fn new_agents(ui: &mut egui::Ui, prefs: &mut Prefs, models: &crate::sessions::Models) {
    let defaults = &mut prefs.new_agent;
    section(ui, "New agents", |ui| {
        setting(
            ui,
            "Agent",
            "Which coding agent a new chat starts with",
            |ui| {
                let before = defaults.provider;
                Segmented::new(
                    &mut defaults.provider,
                    &Provider::ALL.map(|provider| (provider, provider.label())),
                )
                .show(ui);
                // a model belongs to one agent, so switching agent goes back to that agent's own default
                if defaults.provider != before {
                    defaults.model = None;
                }
            },
        );
        divider(ui);
        let list = models.get(&defaults.provider);
        let default_name = list
            .and_then(|l| l.iter().find(|m| m.is_default).or(l.first()))
            .map(|m| m.name.clone());
        setting(
            ui,
            "Model",
            "Any chat can still pick another in its composer",
            |ui| {
                let Some(list) = list else {
                    mux_ui::Button::new("Loading…")
                        .size(mux_ui::Size::Sm)
                        .enabled(false)
                        .show(ui);
                    return;
                };
                let current = match &defaults.model {
                    Some(id) => list
                        .iter()
                        .find(|m| &m.id == id)
                        .map_or(id.clone(), |m| m.name.clone()),
                    None => "Default".to_owned(),
                };
                let button = mux_ui::Button::new(current).size(mux_ui::Size::Sm).show(ui);
                egui::Popup::menu(&button)
                    .align(egui::RectAlign::BOTTOM_END)
                    .gap(space::XS)
                    .show(|ui| {
                        ui.set_width(260.0);
                        let detail = default_name.as_deref().unwrap_or_default();
                        if mux_ui::MenuItem::new("Default")
                            .detail(detail)
                            .checked(defaults.model.is_none())
                            .show(ui)
                            .clicked()
                        {
                            defaults.model = None;
                        }
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for model in list {
                                    let chosen =
                                        defaults.model.as_deref() == Some(model.id.as_str());
                                    if mux_ui::MenuItem::new(&model.name)
                                        .checked(chosen)
                                        .show(ui)
                                        .clicked()
                                    {
                                        defaults.model = Some(model.id.clone());
                                    }
                                }
                            });
                    });
            },
        );
        divider(ui);
        setting(ui, "Permissions", defaults.access.description(), |ui| {
            let button = mux_ui::Button::new(defaults.access.label())
                .size(mux_ui::Size::Sm)
                .show(ui);
            egui::Popup::menu(&button)
                .align(egui::RectAlign::BOTTOM_END)
                .gap(space::XS)
                .show(|ui| {
                    ui.set_width(320.0);
                    for access in Access::ALL {
                        if mux_ui::MenuItem::new(access.label())
                            .detail(access.description())
                            .checked(defaults.access == access)
                            .show(ui)
                            .clicked()
                        {
                            defaults.access = access;
                        }
                    }
                });
        });
    });
}

// a captioned card holding a group of settings
pub(crate) fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
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
pub(crate) fn setting(
    ui: &mut egui::Ui,
    title: &str,
    detail: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
    // the control goes first, so the text knows how much room is left beside it
    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    control(&mut controls);
    let used = controls.min_rect().width();
    let room = (rect.width() - used - space::LG).max(0.0);
    // a line too long for the room is cut short rather than running under the control
    let line = |text: &str, font, color| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        ui.painter().layout_job(job)
    };
    let title = line(title, font::medium(text::MD), p.text);
    let detail = line(detail, font::regular(text::SM + 1.0), p.text_faint);
    // the two lines are centred as a block, level with the control beside them
    let gap = 3.0;
    let top = rect.center().y - (title.size().y + gap + detail.size().y) / 2.0;
    let below = top + title.size().y + gap;
    ui.painter().galley(pos2(rect.left(), top), title, p.text);
    ui.painter()
        .galley(pos2(rect.left(), below), detail, p.text_faint);
}

pub(crate) fn divider(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, p.border));
}
