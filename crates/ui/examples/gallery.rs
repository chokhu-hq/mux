use egui::{Color32, CornerRadius, Key, KeyboardShortcut, Modifiers, RichText, Sense, Theme, Vec2};
use mux_core::{SessionKind, SessionStatus};
use mux_ui::chat::{self, ChatAction, ChatItem, ChatView};
use mux_ui::motion::{self, Spring, spring_bool};
use mux_ui::theme::{self, font, radius, space, text};
use mux_ui::window::{self, WindowFrame};
use mux_ui::{
    Badge, Button, Command, CommandPalette, Icon, Kbd, PALETTE_SHORTCUT, Segmented, SessionRow,
    Size, StatusDot, TextInput, Tone, TooltipExt,
};
use mux_ui::{icons, shortcut};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: window::viewport("Mux Gallery", egui::vec2(960.0, 760.0)),
        ..Default::default()
    };
    eframe::run_native(
        "Mux Gallery",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(Gallery::default()))
        }),
    )
}

#[derive(Default)]
struct Gallery {
    query: String,
    selected: usize,
    moved: bool,
    palette: CommandPalette,
    last_command: Option<&'static str>,
    segment: usize,
    chat: Option<Vec<ChatItem>>,
    draft: String,
    thinking: bool,
}

const CMD_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::COMMAND
};
const NEW_AGENT: KeyboardShortcut = shortcut::app(Key::T);
const TOGGLE_THEME: KeyboardShortcut = KeyboardShortcut::new(CMD_SHIFT, Key::L);

const COMMANDS: [Command; 7] = [
    Command::new("new-agent", "New agent session")
        .icon(icons::ROBOT)
        .shortcut(NEW_AGENT),
    Command::new("new-shell", "New shell")
        .icon(icons::TERMINAL_WINDOW)
        .shortcut(shortcut::app(Key::N)),
    Command::new("run-task", "Run task…").icon(icons::PLAY),
    Command::new("toggle-theme", "Toggle light and dark theme")
        .icon(icons::MOON)
        .shortcut(TOGGLE_THEME),
    Command::new("toggle-terminal", "Toggle terminal panel")
        .icon(icons::TERMINAL)
        .shortcut(KeyboardShortcut::new(Modifiers::COMMAND, Key::J)),
    Command::new("settings", "Open settings")
        .icon(icons::GEAR)
        .shortcut(KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma)),
    Command::new("reload", "Reload window").icon(icons::ARROW_CLOCKWISE),
];

const SESSIONS: [(&str, SessionKind, SessionStatus, &str); 5] = [
    (
        "Fix auth redirect",
        SessionKind::Agent,
        SessionStatus::Running,
        "claude · 2m",
    ),
    (
        "cargo test --workspace",
        SessionKind::Task,
        SessionStatus::Failed,
        "~/mux · 5m",
    ),
    (
        "zsh",
        SessionKind::Shell,
        SessionStatus::Idle,
        "~/mux · 12m",
    ),
    (
        "Refactor storage layer",
        SessionKind::Agent,
        SessionStatus::Waiting,
        "codex · needs input",
    ),
    (
        "pnpm build",
        SessionKind::Task,
        SessionStatus::Done,
        "~/web · 1h",
    ),
];

impl eframe::App for Gallery {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        window::CLEAR_COLOR
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        mux_ui::capture::screenshot(&ctx);
        if let Some(mux_ui::Choice::Command(id)) = self.palette.show(&ctx, &COMMANDS, &[]) {
            if id == "toggle-theme" {
                let dark = ctx.theme() == Theme::Dark;
                ctx.set_theme(if dark { Theme::Light } else { Theme::Dark });
            }
            self.last_command = Some(id);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                WindowFrame::new("Mux Gallery").show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(space::XL as i8)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.heading("Components");
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let dark = ui.visuals().dark_mode;
                                            let reduced = motion::reduced(ui.ctx());
                                            if Button::new(if dark { "Light" } else { "Dark" })
                                                .size(Size::Sm)
                                                .show(ui)
                                                .tip_with_shortcut("Toggle theme", TOGGLE_THEME)
                                                .clicked()
                                            {
                                                ui.ctx().set_theme(if dark {
                                                    Theme::Light
                                                } else {
                                                    Theme::Dark
                                                });
                                            }
                                            let label = if reduced {
                                                "Reduced motion: On"
                                            } else {
                                                "Reduced motion: Off"
                                            };
                                            if Button::new(label)
                                                .size(Size::Sm)
                                                .ghost()
                                                .show(ui)
                                                .tip("Skip spring animations")
                                                .clicked()
                                            {
                                                motion::set_reduced(ui.ctx(), !reduced);
                                            }
                                        },
                                    );
                                });

                                section(ui, "Colors", |ui| {
                                    let p = theme::palette(ui);
                                    let swatches = [
                                        ("bg", p.bg),
                                        ("surface", p.surface),
                                        ("elevated", p.elevated),
                                        ("hover", p.hover),
                                        ("active", p.active),
                                        ("border", p.border),
                                        ("text", p.text),
                                        ("muted", p.text_muted),
                                        ("accent", p.accent),
                                        ("success", p.success),
                                        ("warning", p.warning),
                                        ("danger", p.danger),
                                        ("agent", p.agent),
                                        ("shell", p.shell),
                                        ("task", p.task),
                                    ];
                                    ui.horizontal_wrapped(|ui| {
                                        for (name, color) in swatches {
                                            swatch(ui, name, color, p.border_strong);
                                        }
                                    });
                                });

                                section(ui, "Typography", |ui| {
                                    let p = theme::palette(ui);
                                    ui.label(
                                        RichText::new("Title 20 Inter SemiBold")
                                            .font(font::semibold(text::TITLE)),
                                    );
                                    ui.label(
                                        RichText::new("Large 15 Inter Medium")
                                            .font(font::medium(text::LG)),
                                    );
                                    ui.label("Body 13 Inter Regular, the default for most UI text");
                                    ui.label(
                                        RichText::new("Small 11 for metadata")
                                            .size(text::SM)
                                            .color(p.text_faint),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "fn main() { println!(\"JetBrains Mono 12.5\") }",
                                        )
                                        .monospace(),
                                    );
                                    ui.label(
                                        RichText::new("JetBrains Mono Bold ┌─┬─┐ │ ├─┼─┤ └─┴─┘")
                                            .font(font::mono_bold(text::MONO)),
                                    );
                                });

                                section(ui, "Icons", |ui| {
                                    let p = theme::palette(ui);
                                    for fill in [false, true] {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                                            for (name, glyph) in icons::ALL {
                                                Icon::new(glyph)
                                                    .size(18.0)
                                                    .fill(fill)
                                                    .color(p.text_muted)
                                                    .show(ui)
                                                    .tip(name);
                                            }
                                        });
                                        ui.add_space(space::SM);
                                    }
                                    ui.horizontal(|ui| {
                                        Icon::new(icons::GIT_BRANCH).show(ui);
                                        ui.label("Pair an Icon with a label, icons don't mix into plain text");
                                    });
                                });

                                section(ui, "Buttons", |ui| {
                                    for size in [Size::Md, Size::Sm] {
                                        ui.horizontal(|ui| {
                                            Button::new("Primary").primary().size(size).show(ui);
                                            Button::new("Secondary").size(size).show(ui);
                                            Button::new("Ghost").ghost().size(size).show(ui);
                                            Button::new("Danger").danger().size(size).show(ui);
                                            Button::new("Disabled")
                                                .primary()
                                                .size(size)
                                                .enabled(false)
                                                .show(ui);
                                        });
                                    }
                                });

                                section(ui, "Badges", |ui| {
                                    ui.horizontal(|ui| {
                                        for kind in [
                                            SessionKind::Agent,
                                            SessionKind::Shell,
                                            SessionKind::Task,
                                        ] {
                                            Badge::kind(kind).show(ui);
                                        }
                                        Badge::new("Neutral").show(ui);
                                        Badge::new("Accent").tone(Tone::Accent).show(ui);
                                        Badge::new("Success").tone(Tone::Success).show(ui);
                                        Badge::new("Warning").tone(Tone::Warning).show(ui);
                                        Badge::new("Danger").tone(Tone::Danger).show(ui);
                                    });
                                });

                                section(ui, "Status", |ui| {
                                    ui.horizontal(|ui| {
                                        for status in [
                                            SessionStatus::Running,
                                            SessionStatus::Waiting,
                                            SessionStatus::Idle,
                                            SessionStatus::Done,
                                            SessionStatus::Failed,
                                        ] {
                                            StatusDot::new(status).show(ui);
                                            ui.label(status.label());
                                            ui.add_space(space::SM);
                                        }
                                    });
                                });

                                section(ui, "Keyboard", |ui| {
                                    ui.horizontal(|ui| {
                                        if Button::new("Open palette")
                                            .size(Size::Sm)
                                            .show(ui)
                                            .clicked()
                                        {
                                            self.palette.open();
                                        }
                                        if let Some(id) = self.last_command {
                                            ui.label(format!("Last command: {id}"));
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        Kbd::shortcut(ui, &PALETTE_SHORTCUT);
                                        ui.label("Command palette");
                                        ui.add_space(space::LG);
                                        Kbd::shortcut(ui, &NEW_AGENT);
                                        ui.label("New agent session");
                                    });
                                });

                                section(ui, "Motion", |ui| {
                                    if Button::new("Toggle").size(Size::Sm).show(ui).clicked() {
                                        self.moved = !self.moved;
                                    }
                                    for (name, spring) in [
                                        ("Quick", Spring::QUICK),
                                        ("Smooth", Spring::SMOOTH),
                                        ("Snappy", Spring::SNAPPY),
                                        ("Bouncy", Spring::BOUNCY),
                                    ] {
                                        ui.horizontal(|ui| {
                                            ui.add_sized([64.0, 20.0], egui::Label::new(name));
                                            spring_track(ui, name, spring, self.moved);
                                        });
                                    }
                                });

                                section(ui, "Text input", |ui| {
                                    TextInput::new(&mut self.query)
                                        .placeholder("Search sessions…")
                                        .width(320.0)
                                        .show(ui);
                                });

                                section(ui, "Session rows", |ui| {
                                    let p = theme::palette(ui);
                                    egui::Frame::new()
                                        .fill(p.bg)
                                        .stroke(egui::Stroke::new(1.0, p.border))
                                        .corner_radius(CornerRadius::same(radius::LG))
                                        .inner_margin(space::XS)
                                        .show(ui, |ui| {
                                            ui.set_width(280.0);
                                            ui.spacing_mut().item_spacing.y = 2.0;
                                            for (i, (title, kind, status, meta)) in
                                                SESSIONS.iter().enumerate()
                                            {
                                                let row = SessionRow::new(title, *kind, *status)
                                                    .context(meta)
                                                    .time("2m")
                                                    .branch("main")
                                                    .selected(self.selected == i);
                                                if row.show(ui).clicked() {
                                                    self.selected = i;
                                                }
                                            }
                                        });
                                });

                                section(ui, "Segmented", |ui| {
                                    Segmented::new(
                                        &mut self.segment,
                                        &[(0, "Chat"), (1, "Terminal"), (2, "Changes")],
                                    )
                                    .show(ui);
                                });

                                section(ui, "Thinking", |ui| {
                                    ui.set_max_width(320.0);
                                    chat::thinking(ui, 0);
                                });

                                section(ui, "Chat", |ui| {
                                    let p = theme::palette(ui);
                                    let items = self.chat.get_or_insert_with(chat::sample);
                                    egui::Frame::new()
                                        .fill(p.bg)
                                        .stroke(egui::Stroke::new(1.0, p.border))
                                        .corner_radius(CornerRadius::same(radius::LG))
                                        .show(ui, |ui| {
                                            let size = egui::vec2(ui.available_width(), 560.0);
                                            ui.allocate_ui(size, |ui| {
                                                let action = ChatView::new(items, &mut self.draft)
                                                    .running(self.thinking)
                                                    .show(ui);
                                                match action {
                                                    Some(ChatAction::Send(text)) => {
                                                        items.push(ChatItem::User {
                                                            text,
                                                            attachments: Vec::new(),
                                                            queued: false,
                                                        });
                                                        self.thinking = true;
                                                        chat::set_thinking(items, true);
                                                    }
                                                    Some(
                                                        ChatAction::Attach
                                                        | ChatAction::OpenLink(_)
                                                        | ChatAction::Compact
                                                        | ChatAction::OpenDiff(_)
                                                        | ChatAction::Command(_)
                                                        | ChatAction::Answer(..)
                                                        | ChatAction::ImplementPlan
                                                        | ChatAction::Rewind(_)
                                                        | ChatAction::Queue(..)
                                                        | ChatAction::EditQueued(_)
                                                        | ChatAction::Resume
                                                        | ChatAction::StopBackground(_),
                                                    ) => {}
                                                    Some(ChatAction::Stop) => {
                                                        self.thinking = false;
                                                        chat::set_thinking(items, false);
                                                    }
                                                    Some(ChatAction::Decide(i, decision)) => {
                                                        if let Some(ChatItem::Permission(r)) =
                                                            items.get_mut(i)
                                                        {
                                                            r.decision = Some(decision);
                                                        }
                                                    }
                                                    None => {}
                                                }
                                            });
                                        });
                                });
                            });
                    });
                });
            });
    }
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    ui.add_space(space::XL);
    ui.label(
        RichText::new(title.to_uppercase())
            .font(font::semibold(text::SM))
            .color(p.text_faint),
    );
    ui.add_space(space::XS);
    body(ui);
}

fn spring_track(ui: &mut egui::Ui, name: &str, spring: Spring, on: bool) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(320.0, 20.0), Sense::hover());
    let t = spring_bool(
        ui.ctx(),
        ui.scope_id().with(("spring-track", name)),
        on,
        spring,
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::FULL), p.surface);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), 8.0, p.accent);
}

fn swatch(ui: &mut egui::Ui, name: &str, color: Color32, border: Color32) {
    ui.vertical(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(64.0, 40.0), Sense::hover());
        let corner = CornerRadius::same(radius::MD);
        ui.painter().rect_filled(rect, corner, color);
        ui.painter().rect_stroke(
            rect,
            corner,
            egui::Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        ui.label(RichText::new(name).font(font::regular(text::SM)));
    });
}
