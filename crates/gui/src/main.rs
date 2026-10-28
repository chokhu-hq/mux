// release builds on windows are a windowed app, so no console window opens behind them
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Rect, UiBuilder, pos2, vec2};
use mux_core::SessionKind;
use mux_core::protocol::{ClientMsg, PROTOCOL};
use mux_ui::motion::{self, Spring};
use mux_ui::shortcut;
use mux_ui::theme::{self, font, icon, space, text};
use mux_ui::window::{self, TITLEBAR_HEIGHT, WindowFrame};
use mux_ui::{
    Chip, Command, CommandPalette, EmptyState, FileView, FileViewAction, IconButton, ImageViewer,
    MenuItem, PALETTE_SHORTCUT, SIDEBAR_SHORTCUT, Sidebar, TooltipExt, UsageRing, icons,
    link_target, open_in_editor, provider_color, provider_mark,
};

use link::Link;
use mux_core::protocol::{self as wire, AgentSettings, UsageWindow};
use mux_core::{Access, Provider};
use mux_ui::chat::{ChatAction, ChatItem, ChatView, Decision};
use prefs::Prefs;
use sessions::{Models, Sessions};

#[cfg(target_os = "linux")]
mod blur;
mod connection;
#[cfg(target_os = "linux")]
mod drop;
mod link;
mod logs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod notify;
mod prefs;
mod sessions;
mod settings;
#[cfg(unix)]
mod shell_env;
mod terminal;
mod themes;
mod token_usage;
mod update;
#[cfg(windows)]
mod wsl;

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--server") {
        if let Err(err) = mux_server::run() {
            log::error!("{err}");
            eprintln!("mux: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    mux_core::logging::init("app.log");
    log::info!(
        "mux {} ({}) on {} {}",
        mux_core::VERSION,
        update::build(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    // before anything spawns a thread, and before the server is started so it inherits the result
    #[cfg(unix)]
    shell_env::adopt_login_path();
    let mut viewport = window::viewport("Mux", vec2(1200.0, 800.0)).with_app_id("mux");
    if let Ok(icon) =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icons/mux-256.png"))
    {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        // on wayland mux reads the clipboard itself, since egui's fallback can't see pictures
        clipboard_shortcuts: !wayland(),
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
const TERMINAL: KeyboardShortcut = shortcut::app(Key::Backtick);
// alt on linux, since ctrl and shift with a number turn into symbols on most layouts
const JUMP_MODIFIER: Modifiers = if cfg!(target_os = "macos") {
    Modifiers::COMMAND
} else {
    Modifiers::ALT
};
const NUMBER_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

const COMMANDS: [Command; 7] = [
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
    Command::new("toggle-terminal", "Toggle terminal on the right")
        .icon(icons::TERMINAL_WINDOW)
        .shortcut(TERMINAL),
    Command::new("compact", "Compact conversation").icon(icons::ARROWS_IN_LINE_VERTICAL),
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
    themes: themes::Themes,
    // the settings page stands in for the selected session until it's closed
    settings_open: bool,
    settings_tab: settings::Tab,
    // usage is asked for once each time the page opens
    usage_requested: bool,
    // asked for once on the first frame, and kept alive so the compositor keeps blurring
    #[cfg(target_os = "linux")]
    blur: Option<Result<blur::Blur, String>>,
    // started on the first frame, since winit doesn't pass wayland drops on
    #[cfg(target_os = "linux")]
    drops: Option<Option<std::sync::mpsc::Receiver<drop::DropEvent>>>,
    // pastes read straight from the compositor, started alongside the drop listener
    #[cfg(target_os = "linux")]
    clipboard: Option<drop::Clipboard>,
    // files are being dragged over the window
    dropping: bool,
    // files dropped since the last frame, for the open agent to pick up
    dropped: Vec<String>,
    // which agent clis are on PATH, checked once for the welcome screen
    installed: Option<[(Provider, bool); 3]>,
    // the file opened from a chat link, kept while the panel slides shut
    viewer: Option<FileView>,
    viewer_open: bool,
    viewer_width: f32,
    // the terminal pane on the right, and which session last took focus in it
    split_width: f32,
    split_focused: Option<u64>,
    // what the pane last showed, kept while it fades out after closing
    split_shown: Option<u64>,
    // every file changed in the open chat, with the agent's own hunks for when git can't say, and the one shown
    diffs: Vec<(String, String)>,
    diff_index: usize,
    // the title bar's changes were clicked, so the next frame opens the diff panel
    review: bool,
    // sessions whose desktop notification was clicked, sent from the threads waiting on them
    opened: (std::sync::mpsc::Sender<u64>, std::sync::mpsc::Receiver<u64>),
    // a newer release, once the check at launch finds one
    update: Option<update::Update>,
    updates: std::sync::mpsc::Receiver<update::Update>,
    // on windows, the wsl distributions installed, read once when settings first open
    #[cfg(windows)]
    distros: Option<Vec<String>>,
}

struct TitleBar {
    id: u64,
    provider: Option<Provider>,
    project: String,
    title: String,
    branch: Option<String>,
    status: String,
    // files changed in the chat and lines added and removed, which open the diff panel
    changes: Option<(usize, usize, usize)>,
    restartable: bool,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let prefs = Prefs::load();
        // the server's paths are translated for this app once it's known to run in wsl
        #[cfg(windows)]
        mux_core::paths::set_wsl(prefs.wsl.clone());
        let mut app = Self {
            themes: themes::Themes::load(),
            palette: CommandPalette::default(),
            sidebar: Sidebar::default(),
            sessions: Sessions::default(),
            focused: None,
            link: Link::start(ctx),
            titlebar: None,
            prefs,
            settings_open: false,
            settings_tab: settings::Tab::default(),
            usage_requested: false,
            #[cfg(target_os = "linux")]
            blur: None,
            #[cfg(target_os = "linux")]
            drops: None,
            #[cfg(target_os = "linux")]
            clipboard: None,
            dropping: false,
            dropped: Vec::new(),
            installed: None,
            viewer: None,
            viewer_open: false,
            viewer_width: 560.0,
            split_width: 520.0,
            split_focused: None,
            split_shown: None,
            diffs: Vec::new(),
            diff_index: 0,
            review: false,
            opened: std::sync::mpsc::channel(),
            update: None,
            updates: update::check(ctx),
            #[cfg(windows)]
            distros: None,
        };
        app.prefs.apply(ctx);
        app.themes.apply(ctx, &app.prefs);
        app.sessions.agent_defaults = app.prefs.new_agent.settings();
        app
    }

    fn run(&mut self, command: &str) {
        match command {
            "new-agent" => self.sessions.create("New agent", SessionKind::Agent),
            "new-shell" => self.sessions.create("Shell", SessionKind::Shell),
            "new-group" => self.sessions.create_group_from_folder(),
            "toggle-sidebar" => self.sidebar.toggle(),
            "toggle-terminal" => self.sessions.toggle_split_terminal(),
            "settings" => self.settings_open = true,
            "compact" => {
                if let Some((session, _, outbox, _)) = self.sessions.selected()
                    && session.kind == SessionKind::Agent
                {
                    outbox.push(ClientMsg::AgentCompact { id: session.id });
                }
            }
            _ => {}
        }
    }

    fn sidebar_contents(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let rect = ui.max_rect().shrink(space::SM);
        if let Ok(update) = self.updates.try_recv() {
            self.update = Some(update);
        }
        let stale = self.link.is_stale();
        let update_height =
            FOOTER_ROW * (usize::from(self.update.is_some()) + usize::from(stale)) as f32;
        let footer_height = FOOTER_ROW + update_height + space::SM + 1.0;
        let list = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - footer_height));
        let footer = Rect::from_min_max(pos2(rect.min.x, list.max.y), rect.max);

        let mut ui_list = ui.new_child(UiBuilder::new().max_rect(list));
        ui_list.add_space(space::XS);
        if self.sessions.is_loaded() && !self.sessions.has_groups() {
            ui_list.horizontal(|ui| {
                ui.add_space(space::SM);
                ui.label(
                    egui::RichText::new("No projects yet")
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_faint),
                );
            });
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut ui_list, |ui| self.sessions.show(ui));

        let mut ui_footer = ui.new_child(UiBuilder::new().max_rect(footer));
        ui_footer.spacing_mut().item_spacing.y = 0.0;
        let line = ui_footer.available_rect_before_wrap();
        ui_footer
            .painter()
            .hline(line.x_range(), line.top(), egui::Stroke::new(1.0, p.border));
        ui_footer.add_space(space::SM + 1.0);
        if let Some(update) = &self.update
            && notice_row(&mut ui_footer, icons::ARROW_CIRCLE_UP, "Update available")
                .tip(format!(
                    "{} is out. Open the release page to download it.",
                    update.version
                ))
                .clicked()
        {
            mux_ui::chat::open_externally(&update.url);
        }
        // the server waits for a click here while something runs in it, since restarting it stops that
        if stale
            && notice_row(&mut ui_footer, icons::ARROW_CLOCKWISE, "Restart server")
                .tip("The background server is from another build of Mux, so recent fixes aren't in it yet. Restarting it stops the terminals and agent turns running now.")
                .clicked()
        {
            self.link.restart_server();
        }
        // only shown when something is wrong, a working connection needs no badge
        let status = match &self.link {
            Link::Up(_) => None,
            Link::Connecting { .. } | Link::Stopping(_) => Some((p.warning, "Connecting…")),
            Link::Down { .. } => Some((p.danger, "Offline")),
            Link::Outdated { .. } => Some((p.warning, "Server outdated")),
        };
        // the agent closest to its limit stands in for both, with the details a hover away
        let usage = Provider::ALL
            .into_iter()
            .map(|provider| (provider, self.sessions.usage(provider)))
            .filter(|(_, windows)| !windows.is_empty())
            .max_by(|a, b| {
                let most = |w: &[UsageWindow]| w.iter().map(|w| w.used).fold(0.0, f32::max);
                most(a.1).total_cmp(&most(b.1))
            });
        if settings_row(&mut ui_footer, self.settings_open, status, usage).clicked() {
            self.settings_open = !self.settings_open;
        }
    }

    fn main_contents(&mut self, ui: &mut egui::Ui, run: &mut Option<&'static str>) {
        if self.sessions.take_picked() {
            self.settings_open = false;
        }
        if self.settings_open {
            if !std::mem::replace(&mut self.usage_requested, true) {
                self.sessions.outbox.push(ClientMsg::RefreshUsage);
            }
            self.settings_page(ui);
            return;
        }
        self.usage_requested = false;
        let group = self.sessions.selected_group();
        if !self.link.is_up() {
            self.link_view(ui);
            return;
        }
        if !self.sessions.is_loaded() {
            return;
        }
        if !self.sessions.has_groups() {
            let installed = *self.installed.get_or_insert_with(installed_agents);
            if welcome(ui, &installed) {
                *run = Some("new-group");
            }
            return;
        }
        let branch = self.sessions.selected_branch();
        // asked for from the title bar last frame, answered here where the chat is at hand
        let review = std::mem::take(&mut self.review);
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
        let ended = sessions::ended(session.status);
        let agent = session.kind == SessionKind::Agent;
        // an agent names its model rather than just its cli
        let model = models
            .get(&local.settings.provider)
            .and_then(|list| {
                list.iter()
                    .find(|m| Some(&m.id) == local.settings.model.as_ref())
                    .or_else(|| list.iter().find(|m| m.is_default))
            })
            .map(|m| m.name.clone());
        let meta = match (agent, model) {
            (true, Some(model)) => model,
            _ => session.meta.clone(),
        };
        let (mut files, mut added, mut removed) = (Vec::new(), 0, 0);
        for item in &local.chat {
            if let ChatItem::Diff(diff) = item {
                if !files.contains(&&diff.path) {
                    files.push(&diff.path);
                }
                for line in &diff.lines {
                    match line.kind {
                        mux_ui::chat::DiffKind::Add => added += 1,
                        mux_ui::chat::DiffKind::Remove => removed += 1,
                        mux_ui::chat::DiffKind::Context => {}
                    }
                }
            }
        }
        self.titlebar = Some(TitleBar {
            id: session.id,
            provider: agent.then_some(session.provider),
            project: group.as_ref().map(|g| g.2.clone()).unwrap_or_default(),
            title: session.title.clone(),
            branch,
            status: format!("{} · {meta}", session.status.label()),
            changes: (agent && !files.is_empty()).then_some((files.len(), added, removed)),
            restartable: ended && !agent,
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
            let folder = group.as_ref().and_then(|g| g.1.clone());
            let mut body_ui = ui.new_child(UiBuilder::new().max_rect(rect));
            let dropped = std::mem::take(&mut self.dropped);
            let action = agent_chat(
                &mut body_ui,
                AgentPane {
                    id,
                    local,
                    outbox,
                    models,
                    folder: folder.as_deref(),
                },
                self.dropping,
                dropped,
                review,
            );
            match action {
                Some(ChatAction::OpenLink(target)) => {
                    self.open_link(ui.ctx(), &target, folder.as_deref())
                }
                Some(ChatAction::OpenDiff(index)) => {
                    self.diffs = changed_files(&local.chat, folder.as_deref());
                    let clicked = match local.chat.get(index) {
                        Some(ChatItem::Diff(diff)) => Some(resolve(&diff.path, folder.as_deref())),
                        _ => None,
                    };
                    if let Some(at) = clicked.and_then(|p| self.diffs.iter().position(|d| d.0 == p))
                    {
                        self.show_diff(at);
                    }
                }
                Some(ChatAction::Command(_)) => *run = Some("new-agent"),
                _ => {}
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
        let (response, zoom) = terminal.show(&mut body_ui, id, outbox, self.prefs.terminal_size());
        if let Some(size) = zoom {
            zoom_terminal(&mut self.prefs, size);
        }
        // files dropped on a terminal type their paths, quoted, the way other terminals do
        let dropped = std::mem::take(&mut self.dropped);
        if !dropped.is_empty() {
            let typed: Vec<String> = dropped.iter().map(|p| shell_quote(p)).collect();
            outbox.push(ClientMsg::Input {
                id,
                data: format!("{} ", typed.join(" ")).into_bytes(),
            });
        }
        // focus the terminal when switching to it, and take focus back once the palette closes
        let idle = ui.ctx().memory(|m| m.focused().is_none()) && !self.palette.is_open();
        if self.focused != Some(id) || idle {
            response.request_focus();
            self.focused = Some(id);
        }
    }

    // a link in a chat opens a picture in the viewer and anything else in the file panel
    fn open_link(&mut self, ctx: &egui::Context, target: &str, folder: Option<&str>) {
        match link_target(target, folder) {
            Some((path, _)) if mux_core::protocol::is_image(&path) => {
                ImageViewer::open(ctx, vec![mux_core::paths::local(&path)], 0);
            }
            Some((path, lines)) => {
                let path = mux_core::paths::local(&path);
                self.viewer = Some(FileView::open(path, lines));
                self.viewer_open = true;
            }
            None => {}
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
        let area = Rect::from_min_max(
            pos2(left, bar.top()),
            pos2(
                bar.right() - space::MD - window::titlebar_trailing_inset(),
                bar.bottom(),
            ),
        );
        // the status words give way first when the window is narrow, so the title keeps some room
        let status_width = ui
            .painter()
            .layout_no_wrap(
                info.status.clone(),
                font::regular(text::SM + 1.0),
                p.text_faint,
            )
            .size()
            .x;
        let show_status = area.width() - status_width > 520.0;
        // the actions on the right are laid out first, so the crumbs are cut short before them instead of running under
        let mut actions = ui.new_child(
            UiBuilder::new()
                .max_rect(area)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        actions.spacing_mut().item_spacing.x = space::SM;
        if IconButton::new(icons::TERMINAL_WINDOW, "Terminal")
            .selected(self.sessions.split.is_some())
            .show(&mut actions)
            .tip_with_shortcut("Terminal on the right", TERMINAL)
            .clicked()
        {
            *run = Some("toggle-terminal");
        }
        if info.restartable
            && IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                .show(&mut actions)
                .tip("Restart")
                .clicked()
        {
            self.sessions
                .outbox
                .push(ClientMsg::Restart { id: info.id });
        }
        if show_status {
            actions.label(
                egui::RichText::new(&info.status)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        }
        if let (Some(provider), false) = (info.provider, usage.is_empty()) {
            actions.add_space(space::SM);
            UsageRing::new(&usage).provider(provider).show(&mut actions);
        }
        if let Some((files, added, removed)) = info.changes {
            actions.add_space(space::SM);
            if changes_button(&mut actions, added, removed)
                .tip(format!(
                    "{files} {} changed in this chat, click to review",
                    if files == 1 { "file" } else { "files" }
                ))
                .clicked()
            {
                self.review = true;
            }
        }

        let right = (actions.min_rect().left() - space::LG).max(area.left());
        let mut crumbs = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(area.min, pos2(right, area.bottom())))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        crumbs.set_clip_rect(crumbs.max_rect().intersect(crumbs.clip_rect()));
        crumbs.spacing_mut().item_spacing.x = space::SM;
        if !info.project.is_empty() {
            crumbs.add(
                egui::Label::new(
                    egui::RichText::new(&info.project)
                        .font(font::medium(text::MD))
                        .color(p.text_faint),
                )
                .truncate(),
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
        crumbs.add(
            egui::Label::new(
                egui::RichText::new(&info.title)
                    .font(font::medium(text::MD))
                    .color(p.text),
            )
            .truncate(),
        );
        if let Some(branch) = &info.branch
            && crumbs.available_width() > 80.0
        {
            crumbs.add_space(space::XS);
            crumbs.spacing_mut().item_spacing.x = space::XS;
            crumbs.label(
                egui::RichText::new(icons::GIT_BRANCH.to_string())
                    .font(font::icon(icon::SM))
                    .color(p.text_faint),
            );
            crumbs.add(
                egui::Label::new(
                    egui::RichText::new(branch)
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_faint),
                )
                .truncate(),
            );
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !egui::Popup::is_any_open(&ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.settings_open = false;
        }
        #[cfg(target_os = "linux")]
        let blur = matches!(self.blur, Some(Ok(_)));
        #[cfg(not(target_os = "linux"))]
        let blur = false;
        let agents = *self.installed.get_or_insert_with(installed_agents);
        let mut prefs = self.prefs.clone();
        let distros = self.distros();
        let previewed = self.themes.preview.take();
        let close = settings::Page {
            tab: &mut self.settings_tab,
            prefs: &mut prefs,
            blur,
            agents,
            usage: Provider::ALL.map(|p| (p, self.sessions.usage(p).to_vec())),
            tokens: self.sessions.tokens().cloned(),
            distros,
            models: self.sessions.models().clone(),
            themes: &mut self.themes,
        }
        .show(ui);
        if close {
            self.settings_open = false;
            self.themes.preview = None;
        }
        if self.themes.preview != previewed {
            self.themes.apply(&ctx, &self.prefs);
        }
        if prefs != self.prefs {
            let moved = prefs.wsl != self.prefs.wsl;
            self.sessions.agent_defaults = prefs.new_agent.settings();
            prefs.apply(&ctx);
            self.themes.apply(&ctx, &prefs);
            prefs.save();
            self.prefs = prefs;
            if moved {
                self.switch_server(&ctx);
            }
        }
    }

    // a server elsewhere has its own projects and sessions, so everything shown is fetched again from it
    fn switch_server(&mut self, ctx: &egui::Context) {
        log::info!(
            "switching the server to {}",
            self.prefs.wsl.as_deref().map_or("windows", |d| d)
        );
        mux_core::paths::set_wsl(self.prefs.wsl.clone());
        self.sessions = Sessions::default();
        self.sessions.agent_defaults = self.prefs.new_agent.settings();
        self.installed = None;
        self.link = Link::start(ctx);
    }

    #[cfg(windows)]
    fn distros(&mut self) -> Option<Vec<String>> {
        Some(self.distros.get_or_insert_with(wsl::distros).clone())
    }

    #[cfg(not(windows))]
    fn distros(&mut self) -> Option<Vec<String>> {
        None
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
                Some(match protocol {
                    Some(protocol) => format!(
                        "It speaks protocol {protocol} and this app speaks {PROTOCOL}. Restarting it stops the sessions running in it."
                    ),
                    None => "This app can't read what it sends. Restarting it stops the sessions running in it.".to_owned(),
                }),
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

// the first run screen: what mux is, the one thing to do next, and whether the agents are ready
fn welcome(ui: &mut egui::Ui, agents: &[(Provider, bool)]) -> bool {
    let p = theme::palette(ui);
    let width = 420.0;
    let mut open = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.5 - 230.0).max(space::XL));
        let (tile, _) = ui.allocate_exact_size(vec2(56.0, 56.0), egui::Sense::hover());
        let rounding = egui::CornerRadius::same(theme::radius::XL);
        ui.painter()
            .rect_filled(tile, rounding, theme::tint(p, theme::fill::QUIET));
        ui.painter().rect_stroke(
            tile,
            rounding,
            egui::Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            tile.center(),
            egui::Align2::CENTER_CENTER,
            icons::TERMINAL_WINDOW,
            font::icon(28.0),
            p.text,
        );
        ui.add_space(space::LG);
        ui.label(
            egui::RichText::new("Welcome to Mux")
                .font(font::semibold(text::TITLE))
                .color(p.text),
        );
        ui.add_space(space::SM);
        let mut job = egui::text::LayoutJob::simple(
            "Run coding agents, shells and tasks side by side. Each project is a folder, and everything you start in it works inside that folder.".to_owned(),
            font::regular(text::MD),
            p.text_muted,
            width,
        );
        job.halign = egui::Align::Center;
        // laid out up front so it wraps at the card's width rather than the window's
        let galley = ui.painter().layout_job(job);
        ui.add(egui::Label::new(galley));
        ui.add_space(space::XL);
        open = mux_ui::Button::new("Open a project folder…")
            .primary()
            .show(ui)
            .clicked();
        ui.add_space(space::XL);

        ui.allocate_ui(vec2(width, 0.0), |ui| {
            theme::card(ui)
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(width - 28.0);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = space::SM;
                        ui.label(
                            egui::RichText::new("Coding agents")
                                .font(font::medium(text::SM))
                                .color(p.text_faint),
                        );
                        for &(provider, ready) in agents {
                            agent_status(ui, provider, ready);
                        }
                    });
                });
        });
        ui.add_space(space::LG);
        ui.horizontal(|ui| {
            let hint = egui::RichText::new("to see every command")
                .font(font::regular(text::SM + 1.0))
                .color(p.text_faint);
            let lead = egui::RichText::new("Press")
                .font(font::regular(text::SM + 1.0))
                .color(p.text_faint);
            // centred by measuring the row once, since a horizontal layout starts at the left
            let row_id = ui.scope_id().with("hint-width");
            let row = ui.data(|d| d.get_temp::<f32>(row_id)).unwrap_or(0.0);
            ui.add_space(((ui.available_width() - row) / 2.0).max(0.0));
            let start = ui.cursor().min.x;
            ui.label(lead);
            mux_ui::Kbd::shortcut(ui, &PALETTE_SHORTCUT);
            ui.label(hint);
            let measured = ui.min_rect().right() - start;
            ui.data_mut(|d| d.insert_temp(row_id, measured));
        });
    });
    open
}

fn agent_status(ui: &mut egui::Ui, provider: Provider, ready: bool) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(icon::LG, icon::LG), egui::Sense::hover());
        provider_mark(
            ui.painter(),
            mark.center(),
            icon::LG,
            provider,
            provider_color(provider, p.text),
        );
        ui.label(
            egui::RichText::new(provider.label())
                .font(font::medium(text::MD))
                .color(p.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (glyph, label, color) = if ready {
                (icons::CHECK_CIRCLE, "Installed".to_owned(), p.success)
            } else {
                (
                    icons::WARNING_CIRCLE,
                    format!("Install the {} CLI", cli(provider)),
                    p.warning,
                )
            };
            ui.label(
                egui::RichText::new(label)
                    .font(font::regular(text::SM + 1.0))
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

fn cli(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::OpenCode => "opencode",
    }
}

// a terminal asked for a new text size, which every terminal then shares and keeps
fn zoom_terminal(prefs: &mut Prefs, size: f32) {
    let default = (size - theme::text::MONO).abs() < 0.01;
    prefs.terminal_text = (!default).then_some(size);
    prefs.save();
}

// a path the agent reported, made absolute against the project when it's relative
fn resolve(path: &str, folder: Option<&str>) -> String {
    match folder {
        Some(folder) => mux_core::paths::join(folder, path),
        None => path.to_owned(),
    }
}

// each file the chat changed, once and in order, with every hunk the agent showed for it
fn changed_files(chat: &[ChatItem], folder: Option<&str>) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for item in chat {
        let ChatItem::Diff(diff) = item else {
            continue;
        };
        let path = resolve(&diff.path, folder);
        let mut hunk = "@@ -1 +1 @@\n".to_owned();
        for line in &diff.lines {
            let sign = match line.kind {
                mux_ui::chat::DiffKind::Add => '+',
                mux_ui::chat::DiffKind::Remove => '-',
                mux_ui::chat::DiffKind::Context => ' ',
            };
            hunk.push(sign);
            hunk.push_str(&line.text);
            hunk.push('\n');
        }
        match files.iter_mut().find(|(p, _)| *p == path) {
            Some((_, hunks)) => hunks.push_str(&hunk),
            None => files.push((path, hunk)),
        }
    }
    files
}

// the file's changes since the last commit, falling back to the agent's own hunks outside a repository
// git runs where the server does, so it reads the same repository the agent changed
fn file_diff(path: &str, fallback: &str) -> String {
    let dir = mux_core::paths::parent(path);
    let git = |args: &[&str]| {
        mux_core::paths::command("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .ok()
    };
    let text = |out: std::process::Output| String::from_utf8_lossy(&out.stdout).into_owned();
    let inside = git(&["rev-parse", "--is-inside-work-tree"]).is_some_and(|o| o.status.success());
    if !inside {
        return fallback.to_owned();
    }
    if let Some(out) = git(&["diff", "--no-color", "--no-ext-diff", "HEAD", "--", path])
        && out.status.success()
        && !out.stdout.is_empty()
    {
        return text(out);
    }
    // a file git doesn't track yet is all new, and comparing it with nothing shows that
    let tracked =
        git(&["ls-files", "--error-unmatch", "--", path]).is_some_and(|o| o.status.success());
    if !tracked
        && std::path::Path::new(&mux_core::paths::local(path)).is_file()
        && let Some(out) = git(&["diff", "--no-color", "--no-index", "--", "/dev/null", path])
        && !out.stdout.is_empty()
    {
        return text(out);
    }
    // committed since, so what the agent showed is the best account of the change
    fallback.to_owned()
}

fn wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

// copy, cut and paste shortcuts become the events egui would have made, with pastes read from wayland
#[cfg(target_os = "linux")]
fn clipboard_keys(clipboard: Option<&drop::Clipboard>, events: &mut Vec<egui::Event>) {
    let mut out = Vec::with_capacity(events.len());
    for event in events.drain(..) {
        let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = &event
        else {
            out.push(event);
            continue;
        };
        let command = modifiers.command_only();
        match key {
            Key::C if command => out.push(egui::Event::Copy),
            Key::Copy => out.push(egui::Event::Copy),
            Key::X if command => out.push(egui::Event::Cut),
            Key::Cut => out.push(egui::Event::Cut),
            Key::V if command => {
                out.extend(clipboard.map(drop::Clipboard::paste).unwrap_or_default())
            }
            Key::Paste => out.extend(clipboard.map(drop::Clipboard::paste).unwrap_or_default()),
            _ => out.push(event),
        }
    }
    *events = out;
}

// plain paths go as they are, anything with spaces or shell characters is single quoted
fn shell_quote(path: &str) -> String {
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c));
    if plain {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

// the clis are looked for where the server runs, since that's where it starts them
fn installed_agents() -> [(Provider, bool); 3] {
    #[cfg(windows)]
    if let Some(distro) = mux_core::paths::wsl() {
        let found = wsl::installed(&distro, &Provider::ALL.map(cli));
        return Provider::ALL.map(|provider| (provider, found.contains(&cli(provider))));
    }
    Provider::ALL.map(installed)
}

// the server runs agents with the same PATH, so finding the cli here means it can start
fn installed(provider: Provider) -> (Provider, bool) {
    let found = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(cli(provider)).is_file())
    });
    (provider, found)
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

    // the model chip carries the effort too, and a narrow composer drops labels in steps:
    // the permission's name first, then build or plan, then the model's name, leaving their icons
    let label = current.map_or(settings.provider.label(), |m| m.name.as_str());
    let effort = current
        .filter(|m| !m.efforts.is_empty())
        .map(|m| effort_label(settings.effort.as_deref().unwrap_or(&m.default_effort)));
    let full_label = match &effort {
        Some(effort) => format!("{label} · {effort}"),
        None => label.to_owned(),
    };
    let access = settings.access;
    let mode = if settings.plan { "Plan" } else { "Build" };
    let place = if settings.worktree {
        "Worktree"
    } else {
        "Local"
    };
    let gap = ui.spacing().item_spacing.x;
    let separators = if locked { 2.0 } else { 3.0 } * (space::SM + gap);
    let width = |label: &str| Chip::new(label).icon(icons::LIGHTNING).width(ui) + gap;
    let toggle = |label: &str| {
        Chip::new(label)
            .icon(icons::LIGHTNING)
            .toggle(false)
            .width(ui)
            + gap
    };
    let place_width = if locked { 0.0 } else { width(place) };
    let budget = ui.available_width() - separators - place_width;
    let model_full = Chip::new(&full_label).provider(settings.provider).width(ui) + gap;
    let show_access = model_full + toggle(mode) + width(access.label()) <= budget;
    let show_mode = model_full + toggle(mode) + width("") <= budget;
    let show_model = model_full + toggle("") + width("") <= budget;
    let shown = if show_model {
        full_label.clone()
    } else {
        effort.clone().unwrap_or_default()
    };
    let chip = Chip::new(&shown)
        .provider(settings.provider)
        .show(ui)
        .tip(format!("Model: {full_label}"));
    let viewing_id = chip.id.with("viewing");
    // tabs and the more models row act inside the menu, so only picking a model or clicking away closes it
    menu(&chip)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            // a fixed width keeps the menu compact, since the list below would otherwise fill the window
            ui.set_width(300.0);
            // the tabs pick which agent's models are listed, the model picked decides the agent
            let mut viewing = ui
                .data(|d| d.get_temp::<Provider>(viewing_id))
                .unwrap_or(settings.provider);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                // measured once, so the tabs split the row evenly
                let count = Provider::ALL.len() as f32;
                let width = (ui.available_width() - space::XS * (count - 1.0)) / count;
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
                        theme::tint(p, theme::fill::SELECTED),
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

            // the current model's effort sits under the list, so model and effort are picked in one place
            if let Some(model) = current.filter(|m| !m.efforts.is_empty()) {
                let chosen = settings
                    .effort
                    .clone()
                    .unwrap_or_else(|| model.default_effort.clone());
                menu_divider(ui);
                menu_caption(ui, "Reasoning effort");
                let total = model.efforts.len();
                for (i, option) in model.efforts.iter().enumerate() {
                    if MenuItem::new(&effort_label(option))
                        .meter(i + 1, total)
                        .checked(*option == chosen)
                        .show(ui)
                        .clicked()
                    {
                        let mut changed = settings.clone();
                        changed.effort = Some(option.clone());
                        next = Some(changed);
                        ui.close();
                    }
                }
            }
            // fast mode is a faster tier of the same model, so it lives with the model too
            if let Some(detail) = current.and_then(|m| m.fast.as_deref()) {
                menu_divider(ui);
                if MenuItem::new("Fast mode")
                    .icon(icons::LIGHTNING)
                    .detail(detail)
                    .checked(settings.fast)
                    .show(ui)
                    .clicked()
                {
                    let mut changed = settings.clone();
                    changed.fast = !changed.fast;
                    next = Some(changed);
                    ui.close();
                }
            }
        });

    control_separator(ui);
    // build or plan, a toggle since there are only the two
    let plan = Chip::new(if show_mode { mode } else { "" })
        .icon(if settings.plan {
            icons::LIST_CHECKS
        } else {
            icons::ROBOT
        })
        .toggle(settings.plan)
        .show(ui)
        .tip(if settings.plan {
            "Plan mode: the agent plans and waits for you. Click to build instead"
        } else {
            "Build mode: the agent makes changes. Click to plan first"
        });
    if plan.clicked() {
        let mut changed = settings.clone();
        changed.plan = !changed.plan;
        next = Some(changed);
    }

    control_separator(ui);
    let chip = Chip::new(if show_access { access.label() } else { "" })
        .icon(access_icon(access))
        .show(ui)
        .tip(format!("Permissions: {}", access.label()));
    menu(&chip).show(|ui| {
        ui.set_width(320.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        menu_caption(ui, "Permissions");
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

    // where it works can only be picked before the first message
    if !locked {
        control_separator(ui);
        let chip = Chip::new(place)
            .icon(if settings.worktree {
                icons::GIT_BRANCH
            } else {
                icons::FOLDER
            })
            .show(ui)
            .tip("Where the agent works");
        menu(&chip).show(|ui| {
            ui.set_width(300.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            menu_caption(ui, "Where it works");
            for (worktree, label, icon, detail) in [
                (
                    false,
                    "Local",
                    icons::FOLDER,
                    "In the project folder itself",
                ),
                (
                    true,
                    "New worktree",
                    icons::GIT_BRANCH,
                    "On its own branch, in a separate checkout",
                ),
            ] {
                if MenuItem::new(label)
                    .icon(icon)
                    .detail(detail)
                    .checked(settings.worktree == worktree)
                    .show(ui)
                    .clicked()
                {
                    let mut changed = settings.clone();
                    changed.worktree = worktree;
                    next = Some(changed);
                    ui.close();
                }
            }
        });
    }

    next
}

// what typing a slash offers: mux's commands that make sense right now, then the agent's own
fn slash_commands(
    provider: Provider,
    skills: &[mux_core::protocol::Skill],
    busy: bool,
    changed: bool,
) -> Vec<mux_ui::chat::SlashCommand> {
    let builtin = |name: &str, detail: &str| mux_ui::chat::SlashCommand {
        name: name.to_owned(),
        detail: detail.to_owned(),
        builtin: true,
    };
    let mut commands = vec![
        builtin("compact", "Summarise older messages to free up context"),
        builtin("plan", "Turn plan mode on or off"),
        builtin("new", "Start a new agent in this project"),
    ];
    if changed {
        commands.push(builtin(
            "review",
            "Step through the files this chat changed",
        ));
    }
    if busy {
        commands.push(builtin("stop", "Stop what the agent is doing"));
    }
    // a skill without a description of its own at least says where it comes from
    commands.extend(skills.iter().map(|skill| mux_ui::chat::SlashCommand {
        name: skill.name.clone(),
        detail: if skill.description.is_empty() {
            format!("{} {} skill", skill.scope, provider.label())
        } else {
            skill.description.clone()
        },
        builtin: false,
    }));
    commands
}

const FOOTER_ROW: f32 = 32.0;

// a quiet row above settings, there only while a newer release is out
// a row above settings that asks for a click, like an update to download
fn notice_row(ui: &mut egui::Ui, glyph: char, label: &str) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), FOOTER_ROW), egui::Sense::click());
    let hover = motion::spring_bool(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        Spring::QUICK,
    );
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(theme::radius::LG),
        theme::tint(p, theme::fill::HOVER * hover),
    );
    let y = rect.center().y;
    let x = rect.left() + space::MD;
    ui.painter().text(
        pos2(x + icon::MD / 2.0, y),
        egui::Align2::CENTER_CENTER,
        glyph,
        font::icon(icon::MD),
        p.accent,
    );
    ui.painter().text(
        pos2(x + icon::MD + space::SM, y),
        egui::Align2::LEFT_CENTER,
        label,
        font::medium(text::MD),
        p.text_muted.lerp_to_gamma(p.text, hover),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// the sidebar's last row: settings, with plan usage at its end, or the connection when it isn't up
fn settings_row(
    ui: &mut egui::Ui,
    open: bool,
    status: Option<(egui::Color32, &str)>,
    usage: Option<(Provider, &[UsageWindow])>,
) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), FOOTER_ROW), egui::Sense::click());
    let hover = motion::spring_bool(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        Spring::QUICK,
    );
    let shown = if open { 1.0 } else { 0.0 };
    let fill = egui::Color32::TRANSPARENT
        .lerp_to_gamma(theme::tint(p, theme::fill::HOVER), hover)
        .lerp_to_gamma(theme::tint(p, theme::fill::SELECTED), shown);
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(theme::radius::LG), fill);
    let y = rect.center().y;
    let x = rect.left() + space::MD;
    let color = p.text_muted.lerp_to_gamma(p.text, hover.max(shown));
    ui.painter().text(
        pos2(x + icon::MD / 2.0, y),
        egui::Align2::CENTER_CENTER,
        icons::GEAR,
        font::icon(icon::MD),
        color,
    );
    ui.painter().text(
        pos2(x + icon::MD + space::SM, y),
        egui::Align2::LEFT_CENTER,
        "Settings",
        font::medium(text::MD),
        color,
    );
    let right = rect.right() - space::SM;
    if let Some((dot, label)) = status {
        let label = ui.painter().text(
            pos2(right - space::XS, y),
            egui::Align2::RIGHT_CENTER,
            label,
            font::regular(text::SM + 1.0),
            p.text_muted,
        );
        ui.painter()
            .circle_filled(pos2(label.left() - space::SM, y), 3.5, dot);
    } else if let Some((provider, windows)) = usage {
        let slot = Rect::from_min_max(pos2(right - 64.0, rect.top()), pos2(right, rect.bottom()));
        let mut ring = ui.new_child(
            UiBuilder::new()
                .max_rect(slot)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        UsageRing::new(windows).provider(provider).show(&mut ring);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .tip_with_shortcut("Settings", SETTINGS)
}

// the chat's added and removed lines as a quiet button that opens them for review
fn changes_button(ui: &mut egui::Ui, added: usize, removed: usize) -> egui::Response {
    let p = theme::palette(ui);
    let painter = ui.painter();
    let font = font::medium(text::SM + 1.0);
    let plus = painter.layout_no_wrap(format!("+{added}"), font.clone(), p.success);
    let minus = painter.layout_no_wrap(format!("−{removed}"), font, p.danger);
    let width =
        space::SM + icon::SM + space::XS + plus.size().x + space::XS + minus.size().x + space::SM;
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, theme::control::SM), egui::Sense::click());
    let hover = motion::spring_bool(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        Spring::QUICK,
    );
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(theme::radius::MD),
        p.hover.gamma_multiply(hover),
    );
    let y = rect.center().y;
    let mut x = rect.left() + space::SM;
    ui.painter().text(
        pos2(x + icon::SM / 2.0, y),
        egui::Align2::CENTER_CENTER,
        icons::GIT_DIFF,
        font::icon(icon::SM),
        p.text_muted,
    );
    x += icon::SM + space::XS;
    let plus_width = plus.size().x;
    ui.painter()
        .galley(pos2(x, y - plus.size().y / 2.0), plus, p.success);
    x += plus_width + space::XS;
    ui.painter()
        .galley(pos2(x, y - minus.size().y / 2.0), minus, p.danger);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// a thin upright line between groups of composer controls
fn control_separator(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(space::SM, 16.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        egui::Stroke::new(1.0, p.border_strong),
    );
}

fn menu_caption(ui: &mut egui::Ui, label: &str) {
    let p = theme::palette(ui);
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new(label)
                .font(font::medium(text::SM))
                .color(p.text_faint),
        );
    });
    ui.add_space(space::XS);
}

fn menu_divider(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    ui.add_space(space::XS);
    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.top(), egui::Stroke::new(1.0, p.border));
    ui.add_space(space::XS + 1.0);
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
            theme::tint(p, theme::fill::HOVER),
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
    // a terminal on the right beside the session, returning what's left of `main` for it
    fn split_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        let p = theme::palette(ui);
        if let Some(id) = self.sessions.split {
            self.split_shown = Some(id);
        }
        let open = self.sessions.split.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("split-panel"), open, Spring::QUICK)
            .clamp(0.0, 1.0);
        let Some(id) = self.split_shown.filter(|_| open || t > 0.01) else {
            self.split_shown = None;
            self.split_focused = None;
            return main;
        };
        let full = self
            .split_width
            .clamp(280.0, (main.width() * 0.7).max(280.0));
        // the chat takes its new width once, at the start of opening and the end of closing,
        // while the pane fades and eases in over that space, so nothing reflows mid animation
        let edge = main.right() - full;
        let handle = Rect::from_min_max(
            pos2(edge - 3.0, main.top()),
            pos2(edge + 3.0, main.bottom()),
        );
        if open {
            let drag = ui
                .interact(
                    handle,
                    egui::Id::unique("split-resize"),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if drag.dragged() {
                self.split_width = full - drag.drag_delta().x;
            }
        }
        let slide = 24.0 * (1.0 - t);
        let panel = Rect::from_min_size(pos2(edge + slide, main.top()), vec2(full, main.height()));
        let clip = Rect::from_min_max(pos2(edge, main.top()), main.max).intersect(ui.clip_rect());
        let mut pane = ui.new_child(UiBuilder::new().id_salt("split-pane").max_rect(panel));
        pane.set_clip_rect(clip);
        pane.multiply_opacity(t);
        // terminal text needs a steady backdrop, so it never shows the blur behind the window
        pane.painter().rect_filled(panel, 0.0, p.bg);
        pane.painter().vline(
            panel.left(),
            panel.y_range(),
            egui::Stroke::new(1.0, p.border),
        );

        let header = Rect::from_min_size(panel.min, vec2(full, FOOTER_ROW + space::SM));
        let mut close = false;
        let Some((info, local, outbox, models, folder)) = self.sessions.split_session(id) else {
            self.split_shown = None;
            return main;
        };
        let mut head = pane.new_child(
            UiBuilder::new()
                .id_salt("split-header")
                .max_rect(header.shrink2(vec2(space::MD, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let agent = info.kind == SessionKind::Agent;
        if agent {
            let (mark, _) =
                head.allocate_exact_size(vec2(icon::MD, icon::MD), egui::Sense::hover());
            provider_mark(
                head.painter(),
                mark.center(),
                icon::MD,
                info.provider,
                provider_color(info.provider, p.text_muted),
            );
        } else {
            let glyph = match info.kind {
                SessionKind::Task => icons::PLAY,
                _ => icons::TERMINAL,
            };
            head.label(
                egui::RichText::new(glyph.to_string())
                    .font(font::icon(icon::MD))
                    .color(p.text_faint),
            );
        }
        head.label(
            egui::RichText::new(&info.title)
                .font(font::medium(text::MD))
                .color(p.text_muted),
        );
        let ended = sessions::ended(info.status);
        // a shell that exits takes its pane with it, the way it closes a terminal tab
        if ended && info.kind == SessionKind::Shell {
            close = true;
        }
        let mut restart = false;
        head.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close |= IconButton::new(icons::X, "Close")
                .show(ui)
                .tip_with_shortcut("Close the pane", TERMINAL)
                .clicked();
            restart = ended
                && !agent
                && IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                    .show(ui)
                    .tip("Run it again")
                    .clicked();
        });
        pane.painter().hline(
            header.x_range(),
            header.bottom(),
            egui::Stroke::new(1.0, p.border),
        );

        if agent {
            let body = Rect::from_min_max(pos2(panel.left(), header.bottom()), panel.max);
            let mut body_ui = pane.new_child(UiBuilder::new().id_salt("split-body").max_rect(body));
            let action = agent_chat(
                &mut body_ui,
                AgentPane {
                    id,
                    local,
                    outbox,
                    models,
                    folder: folder.as_deref(),
                },
                false,
                Vec::new(),
                false,
            );
            match action {
                Some(ChatAction::OpenLink(target)) => {
                    self.open_link(&ctx, &target, folder.as_deref())
                }
                Some(ChatAction::OpenDiff(index)) => {
                    let chat = self.sessions.split_session(id).map(|s| &s.1.chat);
                    if let Some(chat) = chat {
                        self.diffs = changed_files(chat, folder.as_deref());
                        let clicked = match chat.get(index) {
                            Some(ChatItem::Diff(diff)) => {
                                Some(resolve(&diff.path, folder.as_deref()))
                            }
                            _ => None,
                        };
                        if let Some(at) =
                            clicked.and_then(|p| self.diffs.iter().position(|d| d.0 == p))
                        {
                            self.show_diff(at);
                        }
                    }
                }
                _ => {}
            }
            if close {
                self.sessions.split = None;
            }
            return Rect::from_min_max(main.min, pos2(edge, main.bottom()));
        }
        // attaching starts the process on the server if it has not run yet, then streams it here
        if local.terminal.is_none() && !local.attaching {
            outbox.push(ClientMsg::Attach { id });
            local.attaching = true;
        }
        if let Some(terminal) = &mut local.terminal {
            let body = Rect::from_min_max(pos2(panel.left(), header.bottom()), panel.max)
                .shrink2(vec2(space::MD, space::SM));
            let mut body_ui = pane.new_child(UiBuilder::new().id_salt("split-body").max_rect(body));
            let (response, zoom) =
                terminal.show(&mut body_ui, id, outbox, self.prefs.terminal_size());
            if let Some(size) = zoom {
                zoom_terminal(&mut self.prefs, size);
            }
            // a terminal just opened on the right takes the keyboard, as if it had been clicked
            if open && self.split_focused != Some(id) {
                response.request_focus();
                self.split_focused = Some(id);
            }
        }
        if restart {
            self.sessions.restart(id);
        }
        if close {
            self.sessions.split = None;
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }

    // the file viewer on the right, returning what's left of `main` for the session
    fn file_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        if self.viewer_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.viewer_open = false;
        }
        let open = self.viewer_open && self.viewer.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("file-panel"), open, Spring::SMOOTH)
            .clamp(0.0, 1.0);
        if t <= 0.001 {
            if !open {
                self.viewer = None;
            }
            return main;
        }
        let full = self
            .viewer_width
            .clamp(320.0, (main.width() * 0.7).max(320.0));
        // the session takes its final width at once and the panel slides over that edge,
        // so the chat rewraps a single time instead of on every frame of the slide
        let left = main.right() - full * t;
        let edge = if open { main.right() - full } else { left };
        let handle = Rect::from_min_max(
            pos2(left - 3.0, main.top()),
            pos2(left + 3.0, main.bottom()),
        );
        let drag = ui
            .interact(
                handle,
                egui::Id::unique("file-panel-resize"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if drag.dragged() {
            self.viewer_width = full - drag.drag_delta().x;
        }
        // laid out at full width and slid in from the edge, so the file doesn't reflow while it opens
        let panel = Rect::from_min_size(pos2(left, main.top()), vec2(full, main.height()));
        let mut panel_ui = ui.new_child(UiBuilder::new().id_salt("file-panel").max_rect(panel));
        panel_ui.set_clip_rect(panel.intersect(main).intersect(ui.clip_rect()));
        let action = self.viewer.as_mut().and_then(|v| v.show(&mut panel_ui));
        if let (Some(action), Some(view)) = (action, &self.viewer) {
            match action {
                FileViewAction::Close => self.viewer_open = false,
                FileViewAction::ToggleWrap => {
                    if let Some(view) = &mut self.viewer {
                        view.wrap = !view.wrap;
                    }
                }
                FileViewAction::CopyPath => ctx.copy_text(view.path.clone()),
                FileViewAction::OpenInEditor => open_in_editor(&view.path, view.editor_line()),
                FileViewAction::Step(by) => {
                    let count = self.diffs.len() as isize;
                    if count > 0 {
                        let at = (self.diff_index as isize + by).rem_euclid(count);
                        self.show_diff(at as usize);
                    }
                }
            }
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }

    // agents that finished or need an answer get a dot in the sidebar, and a notification when mux is in the background
    fn alerts(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let looking = self
            .sessions
            .selected_id()
            .filter(|_| focused && !self.settings_open);
        for alert in self.sessions.take_alerts(looking) {
            if focused {
                continue;
            }
            let body = match alert.event {
                sessions::Event::Finished => "Finished and is ready for you",
                sessions::Event::Waiting => "Needs your approval to continue",
                sessions::Event::Failed => "Stopped with an error",
            };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            notify::send(
                &alert.title,
                body,
                alert.id,
                self.opened.0.clone(),
                ctx.clone(),
            );
            // windows has no desktop notifications yet
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let _ = (body, &alert.title);
        }
        for id in self.opened.1.try_iter() {
            self.sessions.select(id);
            self.settings_open = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn show_diff(&mut self, index: usize) {
        let Some((path, fallback)) = self.diffs.get(index) else {
            return;
        };
        let mut view = FileView::diff(mux_core::paths::local(path), &file_diff(path, fallback));
        view.step = Some((index, self.diffs.len()));
        self.viewer = Some(view);
        self.viewer_open = true;
        self.diff_index = index;
    }

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
                let (drops, clipboard) = drop::listen(frame, ctx.clone())
                    .inspect_err(|reason| log::warn!("no file drops or pastes: {reason}"))
                    .ok()?;
                self.clipboard = Some(clipboard);
                Some(drops)
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

    // touchpads report scrolling in points, which the line speed above doesn't reach
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        #[cfg(target_os = "linux")]
        if wayland() {
            clipboard_keys(self.clipboard.as_ref(), &mut raw_input.events);
        }
        for event in &mut raw_input.events {
            if let egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta,
                modifiers,
                ..
            } = event
                && !modifiers.command
            {
                *delta *= 1.4;
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(target_os = "linux")]
        let blurred = {
            let first = self.blur.is_none();
            let blur = self.blur.get_or_insert_with(|| blur::request(frame));
            if first && let Err(reason) = blur {
                log::info!("no background blur: {reason}");
            }
            blur.is_ok()
        };
        #[cfg(not(target_os = "linux"))]
        let blurred = {
            let _ = frame;
            false
        };
        self.take_drops(&ctx, frame);
        ImageViewer::show(&ctx);
        let destinations = self.sessions.destinations();
        let mut run = match self.palette.show(&ctx, &COMMANDS, &destinations) {
            Some(mux_ui::Choice::Command(id)) => Some(id),
            Some(mux_ui::Choice::Destination(id)) => {
                self.sessions.select(id);
                self.settings_open = false;
                None
            }
            None => None,
        };
        // the first nine sessions are a key away, the way tabs are in a browser
        let jump = ctx.input_mut(|i| {
            NUMBER_KEYS
                .iter()
                .position(|key| i.consume_key(JUMP_MODIFIER, *key))
        });
        if let Some(session) = jump.and_then(|n| destinations.get(n)) {
            self.sessions.select(session.id);
            self.settings_open = false;
        }
        self.link.tick(&ctx, &mut self.sessions);
        self.alerts(&ctx);
        if self.themes.refresh() {
            self.themes.apply(&ctx, &self.prefs);
        }
        // a theme being written is checked for edits even while the app is otherwise idle
        let custom = [&self.prefs.dark_theme, &self.prefs.light_theme]
            .into_iter()
            .flatten()
            .any(|id| self.themes.find(id).is_some_and(|c| c.custom));
        if self.settings_open || custom {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
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
                        let main = self.file_panel(ui, main);
                        let main = self.split_panel(ui, main);
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

// an agent session's chat on screen, wherever it's shown
struct AgentPane<'a> {
    id: u64,
    local: &'a mut sessions::Local,
    outbox: &'a mut Vec<ClientMsg>,
    models: &'a sessions::Models,
    folder: Option<&'a str>,
}

// draws an agent's chat and carries out what only concerns that agent, handing back links, diffs and a new agent
fn agent_chat(
    ui: &mut egui::Ui,
    pane: AgentPane,
    dropping: bool,
    dropped: Vec<String>,
    review: bool,
) -> Option<ChatAction> {
    let AgentPane {
        id,
        local,
        outbox,
        models,
        folder,
    } = pane;
    let first_diff = local
        .chat
        .iter()
        .position(|item| matches!(item, ChatItem::Diff(_)));
    if !local.opened {
        outbox.push(ClientMsg::AgentOpen { id });
        local.opened = true;
    }
    let settings = local.settings.clone();
    let started = local.has_started();
    let mut configure = None;
    for path in dropped {
        if !local.attachments.contains(&path) {
            local.attachments.push(path);
        }
    }
    let commands = slash_commands(
        local.settings.provider,
        &local.skills,
        local.busy,
        first_diff.is_some(),
    );
    let action = ChatView::new(&local.chat, &mut local.draft)
        .commands(&commands)
        .queue(&local.queue, local.held)
        .background(&local.background)
        .attachments(&mut local.attachments)
        .dropping(dropping)
        .running(local.busy)
        .context(local.context)
        .controls(|ui| configure = agent_controls(ui, models, &settings, started))
        .show(ui);
    let action = action.or(first_diff.filter(|_| review).map(ChatAction::OpenDiff));
    // mux's own slash commands are the same actions the buttons around the chat take
    let action = match action {
        Some(ChatAction::Command(name)) => match name.as_str() {
            "compact" => Some(ChatAction::Compact),
            "review" => first_diff.map(ChatAction::OpenDiff),
            "stop" => Some(ChatAction::Stop),
            "plan" => {
                let mut settings = local.settings.clone();
                settings.plan = !settings.plan;
                local.settings = settings.clone();
                outbox.push(ClientMsg::AgentConfigure { id, settings });
                None
            }
            "new" => Some(ChatAction::Command(name)),
            _ => None,
        },
        action => action,
    };
    if let Some(settings) = configure {
        local.settings = settings.clone();
        outbox.push(ClientMsg::AgentConfigure { id, settings });
    }
    match action {
        Some(ChatAction::Send(text)) => {
            // a file moved or deleted since it was attached is left out rather than failing the turn
            let mut attachments = std::mem::take(&mut local.attachments);
            attachments.retain(|path| std::path::Path::new(path).exists());
            // picked here, but the agent opens them where the server runs
            let attachments = attachments
                .iter()
                .map(|path| mux_core::paths::remote(path))
                .collect();
            outbox.push(ClientMsg::AgentSend {
                id,
                text,
                attachments,
            });
        }
        Some(ChatAction::Attach) => {
            let folder = folder.map(mux_core::paths::local);
            for path in sessions::pick_files(folder.as_deref()) {
                if !local.attachments.contains(&path) {
                    local.attachments.push(path);
                }
            }
        }
        // these reach beyond this one chat, so the caller carries them out
        action @ Some(ChatAction::OpenLink(_) | ChatAction::OpenDiff(_)) => return action,
        Some(ChatAction::Stop) => outbox.push(ClientMsg::AgentStop { id }),
        // only a new agent is left, which the caller starts
        action @ Some(ChatAction::Command(_)) => return action,
        Some(ChatAction::Queue(index, action)) => {
            outbox.push(ClientMsg::AgentQueue { id, index, action })
        }
        // waiting messages come back into the box to change, ahead of anything already typed, and leave the queue
        Some(ChatAction::EditQueued(mut indices)) => {
            indices.retain(|&i| i < local.queue.len());
            let texts: Vec<String> = indices
                .iter()
                .map(|&i| local.queue[i].text.clone())
                .collect();
            for &i in &indices {
                // they were sent as the server sees them, and the box holds this machine's paths
                for path in &local.queue[i].attachments {
                    let path = mux_core::paths::local(path);
                    if !local.attachments.contains(&path) {
                        local.attachments.push(path);
                    }
                }
            }
            let mut parts = texts;
            if !local.draft.trim().is_empty() {
                parts.push(local.draft.trim_end().to_owned());
            }
            local.draft = parts.join("\n\n");
            indices.sort_unstable();
            for &i in indices.iter().rev() {
                local.queue.remove(i);
                outbox.push(ClientMsg::AgentQueue {
                    id,
                    index: i,
                    action: wire::QueueAction::Remove,
                });
            }
        }
        Some(ChatAction::Resume) => outbox.push(ClientMsg::AgentResume { id }),
        Some(ChatAction::StopBackground(task)) => {
            outbox.push(ClientMsg::AgentStopBackground { id, task })
        }
        Some(ChatAction::Answer(index, answers)) => {
            if let Some(Some(question)) = local.approvals.get(index) {
                outbox.push(ClientMsg::AgentAnswer {
                    id,
                    question: *question,
                    answers,
                });
            }
        }
        // the message goes back in the composer, after anything already typed there, and the chat winds back
        Some(ChatAction::Rewind(index)) => {
            let message = local.chat[..index]
                .iter()
                .filter(|item| matches!(item, ChatItem::User { .. }))
                .count();
            if let Some(ChatItem::User {
                text, attachments, ..
            }) = local.chat.get(index)
            {
                local.draft = if local.draft.trim().is_empty() {
                    text.clone()
                } else {
                    format!("{}\n\n{text}", local.draft.trim_end())
                };
                for path in attachments {
                    if !local.attachments.contains(path) {
                        local.attachments.push(path.clone());
                    }
                }
                outbox.push(ClientMsg::AgentRewind { id, message });
            }
        }
        // carrying out a plan leaves plan mode first, so the agent is free to make the changes
        Some(ChatAction::ImplementPlan) => {
            let mut settings = local.settings.clone();
            settings.plan = false;
            local.settings = settings.clone();
            outbox.push(ClientMsg::AgentConfigure { id, settings });
            outbox.push(ClientMsg::AgentSend {
                id,
                text: "Go ahead and carry out the plan.".to_owned(),
                attachments: Vec::new(),
            });
        }
        Some(ChatAction::Compact) => outbox.push(ClientMsg::AgentCompact { id }),
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
    None
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn quotes_dropped_paths_only_when_needed() {
        assert_eq!(shell_quote("/home/me/a.txt"), "/home/me/a.txt");
        assert_eq!(
            shell_quote("/home/me/My Shot.png"),
            "'/home/me/My Shot.png'"
        );
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }
}
