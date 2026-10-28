use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Rect, UiBuilder, pos2, vec2};
use mux_core::protocol::{ClientMsg, PROTOCOL};
use mux_core::{SessionKind, SessionStatus};
use mux_ui::shortcut;
use mux_ui::theme::{self, font, space, text};
use mux_ui::window::{self, TITLEBAR_HEIGHT, WindowFrame};
use mux_ui::{
    Badge, Chip, Command, CommandPalette, EmptyState, IconButton, MenuItem, SIDEBAR_SHORTCUT,
    Sidebar, TooltipExt, icons,
};

use link::Link;
use mux_core::protocol::{self as wire, AgentSettings, ModelOption};
use mux_ui::chat::{ChatAction, ChatView, Decision};
use sessions::Sessions;

mod connection;
mod link;
mod sessions;
mod terminal;

const HEADER_HEIGHT: f32 = 40.0;

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--server") {
        if let Err(err) = mux_server::run() {
            eprintln!("mux: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: window::viewport("Mux", vec2(1200.0, 800.0)),
        ..Default::default()
    };
    eframe::run_native(
        "Mux",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        }),
    )
}

const NEW_AGENT: KeyboardShortcut = shortcut::app(Key::T);
const NEW_SHELL: KeyboardShortcut = shortcut::app(Key::N);
const SETTINGS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);

const COMMANDS: [Command; 5] = [
    Command::new("new-agent", "New agent session")
        .icon(icons::ROBOT)
        .shortcut(NEW_AGENT),
    Command::new("new-shell", "New shell")
        .icon(icons::TERMINAL_WINDOW)
        .shortcut(NEW_SHELL),
    Command::new("new-group", "New group from folder…").icon(icons::FOLDER),
    Command::new("toggle-sidebar", "Toggle sidebar")
        .icon(icons::SIDEBAR_SIMPLE)
        .shortcut(SIDEBAR_SHORTCUT),
    Command::new("settings", "Open settings")
        .icon(icons::GEAR)
        .shortcut(SETTINGS),
];

struct App {
    palette: CommandPalette,
    sidebar: Sidebar,
    sessions: Sessions,
    // the terminal that last took focus, so switching sessions focuses the new one
    focused: Option<u64>,
    link: Link,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        Self {
            palette: CommandPalette::default(),
            sidebar: Sidebar::default(),
            sessions: Sessions::default(),
            focused: None,
            link: Link::start(ctx),
        }
    }

    fn run(&mut self, command: &str) {
        match command {
            "new-agent" => self.sessions.create("New agent", SessionKind::Agent),
            "new-shell" => self.sessions.create("Shell", SessionKind::Shell),
            "new-group" => self.sessions.create_group_from_folder(),
            "toggle-sidebar" => self.sidebar.toggle(),
            _ => {}
        }
    }

    fn sidebar_contents(&mut self, ui: &mut egui::Ui, run: &mut Option<&'static str>) {
        let p = theme::palette(ui);
        let rect = ui.max_rect().shrink(space::SM);
        let footer_height = 32.0;
        let list = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - footer_height));
        let footer = Rect::from_min_max(pos2(rect.min.x, list.max.y), rect.max);

        let mut ui_list = ui.new_child(UiBuilder::new().max_rect(list));
        ui_list.horizontal(|ui| {
            ui.add_space(space::XS);
            ui.label(
                egui::RichText::new("Sessions")
                    .font(font::medium(text::SM))
                    .color(p.text_faint),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if IconButton::new(icons::PLUS, "New agent session")
                    .show(ui)
                    .tip_with_shortcut("New agent session", NEW_AGENT)
                    .clicked()
                {
                    *run = Some("new-agent");
                }
                if IconButton::new(icons::FOLDER, "New group")
                    .show(ui)
                    .tip("New group from folder")
                    .clicked()
                {
                    *run = Some("new-group");
                }
            });
        });
        ui_list.add_space(space::XS);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut ui_list, |ui| self.sessions.show(ui));

        let mut ui_footer = ui.new_child(
            UiBuilder::new()
                .max_rect(footer)
                .layout(egui::Layout::left_to_right(egui::Align::Max)),
        );
        if IconButton::new(icons::GEAR, "Settings")
            .show(&mut ui_footer)
            .tip_with_shortcut("Settings", SETTINGS)
            .clicked()
        {
            *run = Some("settings");
        }
        // only shown when something is wrong, a working connection needs no badge
        let status = match &self.link {
            Link::Up(_) => None,
            Link::Connecting { .. } | Link::Stopping(_) => Some((p.warning, "Connecting…")),
            Link::Down { .. } => Some((p.danger, "Offline")),
            Link::Outdated { .. } => Some((p.warning, "Server outdated")),
        };
        if let Some((color, label)) = status {
            ui_footer.add_space(space::XS);
            // centred on the gear button rather than the footer's bottom edge
            ui_footer.allocate_ui_with_layout(
                vec2(160.0, 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, color);
                    ui.label(
                        egui::RichText::new(label)
                            .font(font::regular(text::SM))
                            .color(p.text_muted),
                    );
                },
            );
        }
    }

    fn main_contents(&mut self, ui: &mut egui::Ui, run: &mut Option<&'static str>) {
        let p = theme::palette(ui);
        let group = self.sessions.selected_group();
        if !self.link.is_up() {
            self.link_view(ui);
            return;
        }
        if !self.sessions.is_loaded() {
            return;
        }
        if !self.sessions.has_groups() {
            if EmptyState::new(icons::FOLDER_OPEN, "Add your first project")
                .detail("A project is a folder on your computer. Agents, shells and tasks you start in it work inside that folder.")
                .action("Add project folder…")
                .show(ui)
            {
                *run = Some("new-group");
            }
            return;
        }
        let Some((session, local, outbox, models)) = self.sessions.selected() else {
            if EmptyState::new(icons::TERMINAL_WINDOW, "No session selected")
                .detail("Pick a session from the sidebar, or start a new one.")
                .action("New agent session")
                .show(ui)
            {
                *run = Some("new-agent");
            }
            return;
        };

        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_HEIGHT));
        let mut bar = ui.new_child(
            UiBuilder::new()
                .max_rect(header.shrink2(vec2(space::LG, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        bar.label(
            egui::RichText::new(&session.title)
                .font(font::semibold(text::MD))
                .color(p.text),
        );
        Badge::kind(session.kind).show(&mut bar);
        let mut restart = false;
        bar.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // an agent that finished its turn is just waiting for the next message, not ended
            let ended = matches!(session.status, SessionStatus::Done | SessionStatus::Failed);
            if ended && session.kind != SessionKind::Agent {
                restart = IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                    .show(ui)
                    .tip("Restart")
                    .clicked();
            }
            ui.label(
                egui::RichText::new(format!("{} · {}", session.status.label(), session.meta))
                    .font(font::regular(text::SM))
                    .color(p.text_faint),
            );
        });
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            egui::Stroke::new(1.0, p.border),
        );

        let id = session.id;
        if restart {
            outbox.push(ClientMsg::Restart { id });
        }
        if session.kind == SessionKind::Agent {
            if let Some((group, None)) = group {
                let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
                let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
                // agents are sandboxed to their folder, so they never run without one
                let choose = EmptyState::new(icons::FOLDER_OPEN, "Choose a project folder")
                    .detail("Agents can only read and change files inside their group's folder. Pick the project this agent should work on.")
                    .action("Choose folder…")
                    .show(&mut body_ui);
                if choose {
                    self.sessions.set_group_folder(group);
                }
                return;
            }
            if !local.opened {
                outbox.push(ClientMsg::AgentOpen { id });
                local.opened = true;
            }
            let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
            let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
            let settings = local.settings.clone();
            let mut configure = None;
            let action = ChatView::new(&local.chat, &mut local.draft)
                .running(local.busy)
                .controls(|ui| configure = agent_controls(ui, models, &settings))
                .show(&mut body_ui);
            if let Some(settings) = configure {
                local.settings = settings.clone();
                outbox.push(ClientMsg::AgentConfigure { id, settings });
            }
            match action {
                Some(ChatAction::Send(text)) => outbox.push(ClientMsg::AgentSend { id, text }),
                Some(ChatAction::Stop) => outbox.push(ClientMsg::AgentStop { id }),
                Some(ChatAction::Decide(index, decision)) => {
                    if let Some(Some(approval)) = local.approvals.get(index) {
                        outbox.push(ClientMsg::AgentDecide {
                            id,
                            approval: *approval,
                            decision: match decision {
                                Decision::Allow => wire::Decision::Allow,
                                Decision::AlwaysAllow => wire::Decision::AlwaysAllow,
                                Decision::Deny => wire::Decision::Deny,
                            },
                        });
                    }
                }
                None => {}
            }
            self.focused = None;
            return;
        }
        // attaching starts the process on the server if it has not run yet, then streams it here
        if local.terminal.is_none() && !local.attaching {
            outbox.push(ClientMsg::Attach { id });
            local.attaching = true;
        }
        let Some(terminal) = &mut local.terminal else {
            return;
        };

        let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max)
            .shrink2(vec2(space::MD, space::SM));
        let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
        let response = terminal.show(&mut body_ui, id, outbox);
        // focus the terminal when switching to it, and take focus back once the palette closes
        let idle = ui.ctx().memory(|m| m.focused().is_none()) && !self.palette.is_open();
        if self.focused != Some(id) || idle {
            response.request_focus();
            self.focused = Some(id);
        }
    }

    fn link_view(&mut self, ui: &mut egui::Ui) {
        let now = ui.input(|i| i.time);
        let (title, detail, action) = match &self.link {
            Link::Connecting { since, .. } => {
                // most connections land within a frame or two, so don't flash a message for those
                if now - since < 0.4 {
                    return;
                }
                ("Connecting to the Mux server…".to_owned(), None, None)
            }
            Link::Up(_) => return,
            Link::Down { error, retry_at } => (
                "Can't reach the Mux server".to_owned(),
                Some(format!(
                    "{error}. Trying again in {}s.",
                    (retry_at - now).ceil().max(0.0)
                )),
                Some("Reconnect now"),
            ),
            Link::Outdated { protocol, .. } => (
                "The running Mux server is from another version".to_owned(),
                Some(format!(
                    "It speaks protocol {protocol} and this app speaks {PROTOCOL}. Restarting it stops the sessions running in it."
                )),
                Some("Restart server"),
            ),
            Link::Stopping(_) => ("Restarting the Mux server…".to_owned(), None, None),
        };

        let mut state = EmptyState::new(icons::CLOUD, &title);
        if let Some(detail) = detail {
            state = state.detail(detail);
        }
        if let Some(action) = action {
            state = state.action(action);
        }
        let clicked = state.show(ui);
        if clicked {
            match self.link {
                Link::Outdated { .. } => self.link.restart_server(),
                _ => self.link.reconnect_now(ui.ctx()),
            }
        }
    }
}

// the model, effort and speed pickers in the composer, returning new settings when one changes
fn agent_controls(
    ui: &mut egui::Ui,
    models: &[ModelOption],
    settings: &AgentSettings,
) -> Option<AgentSettings> {
    let current = settings
        .model
        .as_deref()
        .and_then(|id| models.iter().find(|m| m.id == id))
        .or_else(|| models.iter().find(|m| m.is_default))
        .or(models.first())?;
    let mut next = None;
    let menu = |response: &egui::Response| {
        egui::Popup::menu(response)
            .align(egui::RectAlign::TOP_START)
            .gap(space::XS)
    };

    let chip = Chip::new(&current.name).show(ui).tip("Model");
    menu(&chip).show(|ui| {
        for model in models {
            let item = MenuItem::new(&model.name)
                .checked(model.id == current.id)
                .show(ui)
                .tip(&model.description);
            if item.clicked() {
                let mut changed = settings.clone();
                changed.model = Some(model.id.clone());
                // keep choices the new model supports, and fall back to its defaults otherwise
                if changed
                    .effort
                    .as_ref()
                    .is_some_and(|e| !model.efforts.contains(e))
                {
                    changed.effort = None;
                }
                changed.fast &= model.fast.is_some();
                next = Some(changed);
                ui.close();
            }
        }
    });

    let effort = settings
        .effort
        .clone()
        .unwrap_or_else(|| current.default_effort.clone());
    let chip = Chip::new(&effort_label(&effort))
        .show(ui)
        .tip("Reasoning effort");
    menu(&chip).show(|ui| {
        for option in &current.efforts {
            if MenuItem::new(&effort_label(option))
                .checked(*option == effort)
                .show(ui)
                .clicked()
            {
                let mut changed = settings.clone();
                changed.effort = Some(option.clone());
                next = Some(changed);
                ui.close();
            }
        }
    });

    if let Some(detail) = &current.fast {
        let fast = IconButton::new(icons::LIGHTNING, "Fast mode")
            .selected(settings.fast)
            .show(ui)
            .tip(format!("Fast mode: {detail}"));
        if fast.clicked() {
            let mut changed = settings.clone();
            changed.fast = !changed.fast;
            next = Some(changed);
        }
    }
    next
}

fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        window::CLEAR_COLOR
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut run = self.palette.show(&ctx, &COMMANDS);
        self.link.tick(&ctx, &mut self.sessions);
        mux_ui::capture::screenshot(&ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let window = ui.max_rect();
                WindowFrame::new("Mux").show(ui, |ui| {
                    let mut sidebar = std::mem::take(&mut self.sidebar);
                    let main = sidebar.show(ui, |ui| self.sidebar_contents(ui, &mut run));
                    self.sidebar = sidebar;
                    let mut ui = ui.new_child(UiBuilder::new().max_rect(main));
                    ui.set_clip_rect(main.intersect(ui.clip_rect()));
                    self.main_contents(&mut ui, &mut run);
                });

                // sits in the titlebar next to the traffic lights, so it stays reachable when collapsed
                let toggle = Rect::from_center_size(
                    pos2(
                        window.min.x + window::titlebar_leading_inset() + 12.0,
                        window.min.y + TITLEBAR_HEIGHT / 2.0,
                    ),
                    vec2(24.0, 24.0),
                );
                let mut ui = ui.new_child(UiBuilder::new().max_rect(toggle));
                if IconButton::new(icons::SIDEBAR_SIMPLE, "Toggle sidebar")
                    .show(&mut ui)
                    .tip_with_shortcut("Toggle sidebar", SIDEBAR_SHORTCUT)
                    .clicked()
                {
                    run = Some("toggle-sidebar");
                }
            });
        if let Some(command) = run {
            self.run(command);
        }
        self.link.tick(&ctx, &mut self.sessions);
    }
}
