use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Rect, UiBuilder, pos2, vec2};
use mux_core::protocol::{ClientMsg, PROTOCOL};
use mux_core::{SessionKind, SessionStatus};
use mux_ui::motion::{self, Spring};
use mux_ui::shortcut;
use mux_ui::theme::{self, font, icon, space, text};
use mux_ui::window::{self, TITLEBAR_HEIGHT, WindowFrame};
use mux_ui::{
    Chip, Command, CommandPalette, EmptyState, IconButton, MenuItem, SIDEBAR_SHORTCUT, Segmented,
    Sidebar, TooltipExt, UsageRing, icons, provider_color, provider_mark,
};

use link::Link;
use mux_core::protocol::{self as wire, AgentSettings};
use mux_core::{Access, Provider};
use mux_ui::chat::{ChatAction, ChatView, Decision};
use prefs::{Appearance, Prefs, Transparency};
use sessions::{Models, Sessions};

#[cfg(target_os = "linux")]
mod blur;
mod connection;
#[cfg(target_os = "linux")]
mod drop;
mod link;
mod prefs;
mod sessions;
mod terminal;

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
    // what the titlebar shows for the selected session, filled in while drawing it
    titlebar: Option<TitleBar>,
    prefs: Prefs,
    // set by the settings command so the gear's popover opens on the next frame
    open_settings: bool,
    // asked for once on the first frame, and kept alive so the compositor keeps blurring
    #[cfg(target_os = "linux")]
    blur: Option<Result<blur::Blur, String>>,
    // started on the first frame, since winit doesn't pass wayland drops on
    #[cfg(target_os = "linux")]
    drops: Option<Option<std::sync::mpsc::Receiver<drop::DropEvent>>>,
    // files are being dragged over the window
    dropping: bool,
    // files dropped since the last frame, for the open agent to pick up
    dropped: Vec<String>,
}

struct TitleBar {
    id: u64,
    provider: Option<Provider>,
    project: String,
    title: String,
    status: String,
    restartable: bool,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let app = Self {
            palette: CommandPalette::default(),
            sidebar: Sidebar::default(),
            sessions: Sessions::default(),
            focused: None,
            link: Link::start(ctx),
            titlebar: None,
            prefs: Prefs::load(),
            open_settings: false,
            #[cfg(target_os = "linux")]
            blur: None,
            #[cfg(target_os = "linux")]
            drops: None,
            dropping: false,
            dropped: Vec::new(),
        };
        app.prefs.apply(ctx);
        app
    }

    fn run(&mut self, command: &str) {
        match command {
            "new-agent" => self.sessions.create("New agent", SessionKind::Agent),
            "new-shell" => self.sessions.create("Shell", SessionKind::Shell),
            "new-group" => self.sessions.create_group_from_folder(),
            "toggle-sidebar" => self.sidebar.toggle(),
            "settings" => self.open_settings = true,
            _ => {}
        }
    }

    fn sidebar_contents(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let rect = ui.max_rect().shrink(space::SM);
        let footer_height = 32.0;
        let list = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - footer_height));
        let footer = Rect::from_min_max(pos2(rect.min.x, list.max.y), rect.max);

        let mut ui_list = ui.new_child(UiBuilder::new().max_rect(list));
        ui_list.add_space(space::XS);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut ui_list, |ui| self.sessions.show(ui));

        let mut ui_footer = ui.new_child(
            UiBuilder::new()
                .max_rect(footer)
                .layout(egui::Layout::left_to_right(egui::Align::Max)),
        );
        #[cfg(target_os = "linux")]
        let blur = matches!(self.blur, Some(Ok(_)));
        #[cfg(not(target_os = "linux"))]
        let blur = false;
        let gear = IconButton::new(icons::GEAR, "Settings")
            .show(&mut ui_footer)
            .tip_with_shortcut("Settings", SETTINGS);
        let open =
            std::mem::take(&mut self.open_settings).then_some(egui::SetOpenCommand::Bool(true));
        let mut changed = self.prefs.clone();
        egui::Popup::menu(&gear)
            .open_memory(open.or(gear.clicked().then_some(egui::SetOpenCommand::Toggle)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::TOP_START)
            .gap(space::XS)
            .show(|ui| settings_menu(ui, &mut changed, blur));
        if changed != self.prefs {
            changed.apply(ui.ctx());
            changed.save();
            self.prefs = changed;
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
        // an agent that finished its turn is just waiting for the next message, not ended
        let ended = matches!(session.status, SessionStatus::Done | SessionStatus::Failed);
        self.titlebar = Some(TitleBar {
            id: session.id,
            provider: (session.kind == SessionKind::Agent).then_some(session.provider),
            project: group.as_ref().map(|g| g.2.clone()).unwrap_or_default(),
            title: session.title.clone(),
            status: format!("{} · {}", session.status.label(), session.meta),
            restartable: ended && session.kind != SessionKind::Agent,
        });
        let id = session.id;
        if session.kind == SessionKind::Agent {
            if let Some((group, None, _)) = group {
                let body = rect;
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
            let body = rect;
            let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
            let settings = local.settings.clone();
            let started = local.has_started();
            let mut configure = None;
            for path in std::mem::take(&mut self.dropped) {
                if !local.attachments.contains(&path) {
                    local.attachments.push(path);
                }
            }
            let action = ChatView::new(&local.chat, &mut local.draft)
                .attachments(&mut local.attachments)
                .dropping(self.dropping)
                .running(local.busy)
                .context(local.context)
                .controls(|ui| configure = agent_controls(ui, models, &settings, started))
                .show(&mut body_ui);
            if let Some(settings) = configure {
                local.settings = settings.clone();
                outbox.push(ClientMsg::AgentConfigure { id, settings });
            }
            match action {
                Some(ChatAction::Send(text)) => outbox.push(ClientMsg::AgentSend {
                    id,
                    text,
                    attachments: std::mem::take(&mut local.attachments),
                }),
                Some(ChatAction::Attach) => {
                    let folder = group.as_ref().and_then(|g| g.1.as_deref());
                    for path in sessions::pick_files(folder) {
                        if !local.attachments.contains(&path) {
                            local.attachments.push(path);
                        }
                    }
                }
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

        // terminal text needs a steady backdrop, so it never shows the blur behind the window
        ui.painter().rect_filled(rect, 0.0, theme::palette(ui).bg);
        let body = rect.shrink2(vec2(space::MD, space::SM));
        let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
        let response = terminal.show(&mut body_ui, id, outbox);
        // focus the terminal when switching to it, and take focus back once the palette closes
        let idle = ui.ctx().memory(|m| m.focused().is_none()) && !self.palette.is_open();
        if self.focused != Some(id) || idle {
            response.request_focus();
            self.focused = Some(id);
        }
    }

    // window controls next to the traffic lights, then the session's breadcrumb and status over the content
    fn titlebar_contents(
        &mut self,
        ui: &mut egui::Ui,
        window: Rect,
        sidebar_width: f32,
        run: &mut Option<&'static str>,
    ) {
        let p = theme::palette(ui);
        let bar = Rect::from_min_size(window.min, vec2(window.width(), TITLEBAR_HEIGHT));
        let start = window.min.x + window::titlebar_leading_inset();
        let mut controls = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(start, bar.top()),
                    pos2(start + 60.0, bar.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        controls.spacing_mut().item_spacing.x = 2.0;
        if IconButton::new(icons::SIDEBAR_SIMPLE, "Toggle sidebar")
            .show(&mut controls)
            .tip_with_shortcut("Toggle sidebar", SIDEBAR_SHORTCUT)
            .clicked()
        {
            *run = Some("toggle-sidebar");
        }
        if IconButton::new(icons::FOLDER, "Add project")
            .show(&mut controls)
            .tip("Add project folder")
            .clicked()
        {
            *run = Some("new-group");
        }

        let Some(info) = self.titlebar.take() else {
            return;
        };
        let usage = info
            .provider
            .map(|provider| self.sessions.usage(provider).to_vec())
            .unwrap_or_default();
        let left =
            (window.min.x + sidebar_width + space::LG).max(controls.min_rect().right() + space::MD);
        let mut crumbs = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(left, bar.top()),
                    pos2(bar.right() - space::MD, bar.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        crumbs.spacing_mut().item_spacing.x = space::SM;
        if !info.project.is_empty() {
            crumbs.label(
                egui::RichText::new(&info.project)
                    .font(font::medium(text::MD))
                    .color(p.text_faint),
            );
            crumbs.label(
                egui::RichText::new("›")
                    .font(font::regular(text::MD))
                    .color(p.text_faint),
            );
        }
        if let Some(provider) = info.provider {
            let (rect, _) =
                crumbs.allocate_exact_size(vec2(icon::MD, icon::MD), egui::Sense::hover());
            provider_mark(
                crumbs.painter(),
                rect.center(),
                icon::MD,
                provider,
                provider_color(provider, p.text_muted),
            );
        }
        crumbs.label(
            egui::RichText::new(&info.title)
                .font(font::medium(text::MD))
                .color(p.text),
        );
        crumbs.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if info.restartable
                && IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                    .show(ui)
                    .tip("Restart")
                    .clicked()
            {
                self.sessions
                    .outbox
                    .push(ClientMsg::Restart { id: info.id });
            }
            ui.label(
                egui::RichText::new(&info.status)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
            if let (Some(provider), false) = (info.provider, usage.is_empty()) {
                ui.add_space(space::SM);
                UsageRing::new(&usage).provider(provider).show(ui);
            }
        });
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

const SHOWN_MODELS: usize = 5;

fn settings_menu(ui: &mut egui::Ui, prefs: &mut Prefs, blur: bool) {
    let p = theme::palette(ui);
    ui.set_width(240.0);
    let caption = |ui: &mut egui::Ui, label: &str| {
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.add_space(space::SM);
            ui.label(
                egui::RichText::new(label)
                    .font(font::medium(text::SM))
                    .color(p.text_faint),
            );
        });
    };
    caption(ui, "Appearance");
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
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
    caption(ui, "Motion");
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        Segmented::new(
            &mut prefs.reduced_motion,
            &[(false, "Full"), (true, "Reduced")],
        )
        .show(ui);
    });
    // only offered where the compositor can blur behind the window
    if blur {
        caption(ui, "Transparency");
        ui.horizontal(|ui| {
            ui.add_space(space::SM);
            Segmented::new(
                &mut prefs.transparency,
                &[
                    (Transparency::Off, "Off"),
                    (Transparency::Subtle, "Subtle"),
                    (Transparency::Full, "Full"),
                ],
            )
            .show(ui);
        });
    }
    ui.add_space(space::XS);
}

// the agent, model, effort and speed pickers in the composer, returning new settings when one changes
fn agent_controls(
    ui: &mut egui::Ui,
    models: &Models,
    settings: &AgentSettings,
    locked: bool,
) -> Option<AgentSettings> {
    let p = theme::palette(ui);
    let pick = |provider: Provider| {
        let list = models.get(&provider)?;
        settings
            .model
            .as_deref()
            .filter(|_| provider == settings.provider)
            .and_then(|id| list.iter().find(|m| m.id == id))
            .or_else(|| list.iter().find(|m| m.is_default))
            .or(list.first())
    };
    let current = pick(settings.provider);
    let mut next = None;
    let menu = |response: &egui::Response| {
        egui::Popup::menu(response)
            .align(egui::RectAlign::TOP_START)
            .gap(space::XS)
    };

    let label = current.map_or(settings.provider.label(), |m| m.name.as_str());
    let chip = Chip::new(label)
        .provider(settings.provider)
        .show(ui)
        .tip("Agent and model");
    let viewing_id = chip.id.with("viewing");
    // tabs and the more models row act inside the menu, so only picking a model or clicking away closes it
    menu(&chip)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            // a fixed width keeps the menu compact, since the list below would otherwise fill the window
            ui.set_width(260.0);
            // the tabs pick which agent's models are listed, the model picked decides the agent
            let mut viewing = ui
                .data(|d| d.get_temp::<Provider>(viewing_id))
                .unwrap_or(settings.provider);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                // measured once, so both tabs split the row evenly
                let width = (ui.available_width() - space::XS) / 2.0;
                // the highlight is drawn into this slot after the tabs, so it sits beneath their labels
                let thumb = ui.painter().add(egui::Shape::Noop);
                let mut tabs = Vec::new();
                for provider in Provider::ALL {
                    let tab = agent_tab(ui, provider, viewing == provider, width);
                    if tab.clicked() {
                        viewing = provider;
                    }
                    tabs.push(tab.rect);
                }
                let index = Provider::ALL
                    .iter()
                    .position(|p| *p == viewing)
                    .unwrap_or(0);
                let at = motion::spring(
                    ui.ctx(),
                    viewing_id.with("thumb"),
                    index as f32,
                    Spring::SNAPPY,
                );
                let step = tabs[1].left() - tabs[0].left();
                ui.painter().set(
                    thumb,
                    egui::Shape::rect_filled(
                        tabs[0].translate(vec2(step * at, 0.0)),
                        egui::CornerRadius::same(theme::radius::MD),
                        theme::tint(p, 0.10),
                    ),
                );
            });
            // the list fades in after a tab switch instead of swapping in a single frame
            let switched_id = viewing_id.with("switched");
            let now = ui.input(|i| i.time);
            let last = ui.data(|d| d.get_temp::<Provider>(viewing_id));
            if last.is_some_and(|last| last != viewing) {
                ui.data_mut(|d| d.insert_temp(switched_id, now));
            }
            ui.data_mut(|d| d.insert_temp(viewing_id, viewing));
            let since = now - ui.data(|d| d.get_temp::<f64>(switched_id).unwrap_or(f64::MIN));
            let fade = if motion::reduced(ui.ctx()) {
                1.0
            } else {
                (since / 0.15).clamp(0.0, 1.0) as f32
            };
            if fade < 1.0 {
                ui.ctx().request_repaint();
            }
            ui.multiply_opacity(fade * fade * (3.0 - 2.0 * fade));
            ui.add_space(space::XS);

            if locked && viewing != settings.provider {
                picker_note(
                    ui,
                    viewing,
                    &format!("This chat runs on {}", settings.provider.label()),
                    &format!("Start a new agent to use {}", viewing.label()),
                );
                return;
            }
            let Some(list) = models.get(&viewing) else {
                picker_note(
                    ui,
                    viewing,
                    &format!("Loading {} models…", viewing.label()),
                    "",
                );
                return;
            };
            let chosen = pick(viewing).filter(|_| viewing == settings.provider);
            // the newest few come first in both clis, and older ones wait behind a disclosure row
            let more_id = viewing_id.with(("more", viewing));
            let expanded = ui.data(|d| d.get_temp::<bool>(more_id).unwrap_or(false));
            let current_index = chosen.and_then(|c| list.iter().position(|m| m.id == c.id));
            let shown = if expanded {
                list.len()
            } else {
                // the model in use always stays visible, even when it's an older one
                SHOWN_MODELS
                    .max(current_index.map_or(0, |i| i + 1))
                    .min(list.len())
            };
            ui.spacing_mut().item_spacing.y = 0.0;
            // a popup sizes itself from its last frame, so the list gets an explicit height rather than shrinking to fit
            let size = vec2(
                ui.available_width(),
                (shown as f32 * theme::control::MD).min(280.0),
            );
            ui.allocate_ui(size, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for model in &list[..shown] {
                            let item = MenuItem::new(&model.name)
                                .checked(chosen.is_some_and(|c| c.id == model.id))
                                .show(ui)
                                .tip(&model.description);
                            if item.clicked() {
                                let mut changed = settings.clone();
                                // effort and speed carry over only when the new model supports them
                                if changed.provider != viewing
                                    || changed
                                        .effort
                                        .as_ref()
                                        .is_some_and(|e| !model.efforts.contains(e))
                                {
                                    changed.effort = None;
                                }
                                changed.fast &= model.fast.is_some() && changed.provider == viewing;
                                changed.provider = viewing;
                                changed.model = Some(model.id.clone());
                                next = Some(changed);
                                ui.close();
                            }
                        }
                    });
            });
            let hidden = list.len() - shown;
            if hidden > 0 || expanded {
                let label = if expanded {
                    "Fewer models".to_owned()
                } else {
                    format!("{hidden} more models")
                };
                let caret = if expanded {
                    icons::CARET_UP
                } else {
                    icons::CARET_DOWN
                };
                if MenuItem::new(&label).icon(caret).show(ui).clicked() {
                    ui.data_mut(|d| d.insert_temp(more_id, !expanded));
                }
            }
        });

    let Some(current) = current else {
        return next;
    };
    if !current.efforts.is_empty() {
        let effort = settings
            .effort
            .clone()
            .unwrap_or_else(|| current.default_effort.clone());
        let chip = Chip::new(&effort_label(&effort))
            .show(ui)
            .tip("Reasoning effort");
        menu(&chip).show(|ui| {
            ui.set_width(200.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.add_space(space::SM);
                ui.label(
                    egui::RichText::new("Reasoning effort")
                        .font(font::medium(text::SM))
                        .color(p.text_faint),
                );
            });
            ui.add_space(space::XS);
            let total = current.efforts.len();
            for (i, option) in current.efforts.iter().enumerate() {
                if MenuItem::new(&effort_label(option))
                    .meter(i + 1, total)
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
    }

    let access = settings.access;
    let chip = Chip::new(access.label())
        .icon(access_icon(access))
        .show(ui)
        .tip("What the agent may do without asking");
    menu(&chip).show(|ui| {
        ui.set_width(320.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.add_space(space::SM);
            ui.label(
                egui::RichText::new("Permissions")
                    .font(font::medium(text::SM))
                    .color(p.text_faint),
            );
        });
        ui.add_space(space::XS);
        for option in Access::ALL {
            if MenuItem::new(option.label())
                .icon(access_icon(option))
                .detail(option.description())
                .checked(option == access)
                .show(ui)
                .clicked()
            {
                let mut changed = settings.clone();
                changed.access = option;
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

// fills the model list's usual space with a short centred message, so switching tabs never resizes the menu
fn picker_note(ui: &mut egui::Ui, provider: Provider, title: &str, detail: &str) {
    let p = theme::palette(ui);
    let size = vec2(
        ui.available_width(),
        SHOWN_MODELS as f32 * theme::control::MD,
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let center = rect.center();
    let lines = if detail.is_empty() { 0.0 } else { 18.0 };
    let mark = egui::pos2(center.x, center.y - 14.0 - lines / 2.0);
    provider_mark(
        ui.painter(),
        mark,
        icon::LG + 4.0,
        provider,
        provider_color(provider, p.text_faint).gamma_multiply(0.7),
    );
    ui.painter().text(
        egui::pos2(center.x, mark.y + 22.0),
        egui::Align2::CENTER_CENTER,
        title,
        font::medium(text::MD),
        p.text,
    );
    if !detail.is_empty() {
        ui.painter().text(
            egui::pos2(center.x, mark.y + 22.0 + lines),
            egui::Align2::CENTER_CENTER,
            detail,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
    }
}

fn access_icon(access: Access) -> char {
    match access {
        Access::Supervised => icons::LOCK_SIMPLE,
        Access::AutoEdits => icons::PENCIL_SIMPLE_LINE,
        Access::Auto => icons::SPARKLE,
        Access::Full => icons::LOCK_SIMPLE_OPEN,
    }
}

// one of the agent tabs at the top of the model menu, with the agent's own logo
fn agent_tab(ui: &mut egui::Ui, provider: Provider, active: bool, width: f32) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), egui::Sense::click());
    // the active tab's highlight slides in from the row, so a tab only paints its own hover
    if !active && response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(theme::radius::MD),
            theme::tint(p, 0.05),
        );
    }
    let text = if active { p.text } else { p.text_muted };
    let label =
        ui.painter()
            .layout_no_wrap(provider.label().to_owned(), font::medium(text::MD), text);
    let content = icon::MD + space::SM + label.size().x;
    let left = rect.center().x - content / 2.0;
    provider_mark(
        ui.painter(),
        pos2(left + icon::MD / 2.0, rect.center().y),
        icon::MD,
        provider,
        provider_color(provider, text),
    );
    ui.painter().galley(
        pos2(
            left + icon::MD + space::SM,
            rect.center().y - label.size().y / 2.0,
        ),
        label,
        text,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn effort_label(effort: &str) -> String {
    match effort {
        "" => "Auto".to_owned(),
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

impl App {
    fn take_drops(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // other platforms get drops through egui, wayland through our own listener
        let (hovered, dropped) = ctx.input(|i| {
            let dropped: Vec<String> = i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path())
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            (!i.raw.hovered_files.is_empty(), dropped)
        });
        self.dropped.extend(dropped);
        #[cfg(target_os = "linux")]
        {
            let drops = self.drops.get_or_insert_with(|| {
                drop::listen(frame, ctx.clone())
                    .inspect_err(|reason| eprintln!("mux: no file drops: {reason}"))
                    .ok()
            });
            for event in drops.iter().flat_map(|rx| rx.try_iter()) {
                match event {
                    drop::DropEvent::Hovering(hovering) => self.dropping = hovering,
                    drop::DropEvent::Files(files) => self.dropped.extend(files),
                }
            }
            self.dropping |= hovered;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = frame;
            self.dropping = hovered;
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        window::CLEAR_COLOR
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(target_os = "linux")]
        let blurred = {
            let first = self.blur.is_none();
            let blur = self.blur.get_or_insert_with(|| blur::request(frame));
            if first && let Err(reason) = blur {
                eprintln!("mux: no background blur: {reason}");
            }
            blur.is_ok()
        };
        #[cfg(not(target_os = "linux"))]
        let blurred = {
            let _ = frame;
            false
        };
        self.take_drops(&ctx, frame);
        let mut run = self.palette.show(&ctx, &COMMANDS);
        self.link.tick(&ctx, &mut self.sessions);
        mux_ui::capture::screenshot(&ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let window = ui.max_rect();
                self.titlebar = None;
                let sidebar_width = self.sidebar.animate(&ctx);
                let opacity = if blurred {
                    self.prefs.transparency.opacity()
                } else {
                    (1.0, 1.0)
                };
                #[cfg(target_os = "linux")]
                if let Some(Ok(blur)) = &mut self.blur {
                    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    let corner = if maximized {
                        0.0
                    } else {
                        f32::from(theme::radius::LG)
                    };
                    // the whole window is blurred once the content is see-through too
                    let width = match opacity {
                        (_, content) if content < 1.0 => window.width(),
                        (sidebar, _) if sidebar < 1.0 => sidebar_width,
                        _ => 0.0,
                    };
                    blur.fit(width, window.height(), corner);
                }
                WindowFrame::new("")
                    .sidebar(sidebar_width)
                    .opacity(opacity.0, opacity.1)
                    .show(ui, |ui| {
                        let mut sidebar = std::mem::take(&mut self.sidebar);
                        let main = sidebar.show(ui, |ui| self.sidebar_contents(ui));
                        self.sidebar = sidebar;
                        let mut ui = ui.new_child(UiBuilder::new().max_rect(main));
                        ui.set_clip_rect(main.intersect(ui.clip_rect()));
                        self.main_contents(&mut ui, &mut run);
                    });
                self.titlebar_contents(ui, window, sidebar_width, &mut run);
            });
        if let Some(command) = run {
            self.run(command);
        }
        self.link.tick(&ctx, &mut self.sessions);
    }
}
