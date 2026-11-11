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

use chat::{AgentPane, agent_chat};
use link::Link;
use mux_core::Provider;
use mux_core::protocol::{self as wire, AgentSettings, UsageWindow};
use mux_ui::chat::{ChatAction, ChatItem, ChatView, Decision};
use prefs::Prefs;
use renderer::{SEE_THROUGH, renderer, see_through_renderer};
#[cfg(windows)]
use renderer::{renderer_works, try_see_through};
use sessions::Sessions;

#[cfg(any(windows, target_os = "macos"))]
mod backdrop;
#[cfg(target_os = "linux")]
mod blur;
mod chat;
mod connection;
#[cfg(target_os = "linux")]
mod drop;
mod install;
mod link;
mod logs;
mod mcp;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod notify;
mod picker;
mod prefs;
mod renderer;
mod sessions;
mod settings;
#[cfg(unix)]
mod shell_env;
mod split;
mod terminal;
mod themes;
mod titlebar;
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
    // a build installed by the updater runs from here on, so what the last update left behind can go
    if update::is_release() {
        install::clean_up();
    }
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
    #[cfg(windows)]
    adopt_windows_path();
    #[cfg(windows)]
    if !try_see_through() {
        SEE_THROUGH.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    let viewport = window::viewport("Mux", vec2(1200.0, 800.0)).with_app_id("mux");
    // on macos the bundle's icon is used, since one set here replaces it in the dock and skips apple's sizing
    #[cfg(not(target_os = "macos"))]
    let viewport = match eframe::icon_data::from_png_bytes(include_bytes!(
        "../../../assets/icons/mux-256.png"
    )) {
        Ok(icon) => viewport.with_icon(icon),
        Err(_) => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        // on wayland mux reads the clipboard itself, since egui's fallback can't see pictures
        clipboard_shortcuts: !wayland(),
        wgpu_options: renderer(),
        ..Default::default()
    };
    // the see-through renderer draws into a composition visual, which the window's own surface would sit on top of
    #[cfg(windows)]
    egui_winit::NO_REDIRECTION_BITMAP
        .store(see_through_renderer(), std::sync::atomic::Ordering::Relaxed);
    let start = || -> eframe::AppCreator<'static> {
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            mux_ui::script::install(&cc.egui_ctx);
            Ok(Box::new(App::new(&cc.egui_ctx)))
        })
    };
    match eframe::run_native("Mux", options.clone(), start()) {
        // a gpu that can't draw see-through on windows still gets a window, just a solid one
        Err(e) if cfg!(windows) && see_through_renderer() => {
            log::warn!("the see-through renderer didn't start, so the window is solid: {e}");
            SEE_THROUGH.store(false, std::sync::atomic::Ordering::Relaxed);
            #[cfg(windows)]
            egui_winit::NO_REDIRECTION_BITMAP.store(false, std::sync::atomic::Ordering::Relaxed);
            let options = eframe::NativeOptions {
                wgpu_options: Default::default(),
                ..options
            };
            eframe::run_native("Mux", options, start())
        }
        result => result,
    }
}

// picks up folders added to PATH since the user signed in, like npm's once node or an agent cli is installed,
// which an app opened from explorer otherwise can't see until they sign out and back in
#[cfg(windows)]
fn adopt_windows_path() {
    let Some(fresh) = mux_core::windows_path() else {
        return;
    };
    let current = std::env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<&str> = Vec::new();
    for dir in current.split(';').chain(fresh.split(';')) {
        let dir = dir.trim();
        if !dir.is_empty() && !dirs.iter().any(|d| d.eq_ignore_ascii_case(dir)) {
            dirs.push(dir);
        }
    }
    let merged = dirs.join(";");
    if merged != current {
        log::info!("using windows' current PATH: {merged}");
        // nothing else runs yet, since this is called first thing in main before any thread starts
        unsafe { std::env::set_var("PATH", merged) };
    }
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
    Command::new("new-group", "Add project folder…").icon(icons::FOLDER),
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
    // whether the system's frosted background is on, or why it can't be, once it's first wanted
    #[cfg(any(windows, target_os = "macos"))]
    backdrop: Option<Result<bool, String>>,
    // whether windows was last told the window is dark, so it's only told again when that changes
    #[cfg(windows)]
    told_dark: Option<bool>,
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
    // which agent clis are on PATH and signed in, checked for the welcome screen and settings
    installed: Option<[(Provider, AgentState); 3]>,
    // sign-in checks still running, since each cli takes a moment to answer
    signins: Option<std::sync::mpsc::Receiver<(Provider, bool)>>,
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
    // that update being downloaded and put in place
    installer: Option<install::Installer>,
    updates: std::sync::mpsc::Receiver<update::Update>,
    // on windows, the wsl distributions installed, read once when settings first open
    #[cfg(windows)]
    distros: wsl::Distros,
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
            #[cfg(any(windows, target_os = "macos"))]
            backdrop: None,
            #[cfg(windows)]
            told_dark: None,
            #[cfg(target_os = "linux")]
            drops: None,
            #[cfg(target_os = "linux")]
            clipboard: None,
            dropping: false,
            dropped: Vec::new(),
            installed: None,
            signins: None,
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
            installer: None,
            updates: update::check(ctx),
            #[cfg(windows)]
            distros: wsl::Distros::default(),
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
            "mcp" => {
                self.settings_open = true;
                self.settings_tab = settings::Tab::Mcp;
            }
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
            // with updates on, a signed build for this platform downloads and goes in place by itself
            if let Some(download) = update.download.clone()
                && !self.prefs.manual_updates
                && install::target().is_ok()
            {
                self.installer = Some(install::Installer::start(ui.ctx(), download));
            }
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
        if let Some(update) = &self.update {
            self.update_row(&mut ui_footer, update.clone());
        }
        // the server waits for a click here while something runs in it, since restarting it stops that
        if stale
            && notice_row(&mut ui_footer, icons::ARROW_CLOCKWISE, "Restart server")
                .tip("The background server is from another build of Mux. Restarting it stops running terminals and agents.")
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
        let resets = usage.map_or(0, |(provider, _)| self.sessions.resets_left(provider));
        if settings_row(&mut ui_footer, self.settings_open, status, usage, resets).clicked() {
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
                // someone may have signed in or installed an agent since, so it's checked again
                self.installed = None;
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
            let installed = self.agents(ui.ctx());
            if welcome(ui, &installed) {
                *run = Some("new-group");
            }
            return;
        }
        let branch = self.sessions.selected_branch();
        // asked for from the title bar last frame, answered here where the chat is at hand
        let review = std::mem::take(&mut self.review);
        let codex_reset = self.sessions.reset_offer(Provider::Codex);
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
        let (files, added, removed) = local.changes;
        self.titlebar = Some(TitleBar {
            id: session.id,
            provider: agent.then_some(session.provider),
            project: group.as_ref().map(|g| g.2.clone()).unwrap_or_default(),
            title: session.title.clone(),
            branch,
            status: format!("{} · {meta}", session.status.label()),
            changes: (agent && files > 0).then_some((files, added, removed)),
            restartable: ended && !agent,
        });
        let id = session.id;
        if session.kind == SessionKind::Agent {
            if let Some((group, None, _)) = group {
                let body = rect;
                let mut body_ui = ui.new_child(UiBuilder::new().max_rect(body));
                // agents are sandboxed to their folder, so they never run without one
                let choose = EmptyState::new(icons::FOLDER_OPEN, "Choose a project folder")
                    .detail("Agents can only read and change files inside their project's folder. Pick the folder this agent should work on.")
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
            let reset = codex_reset.filter(|_| local.settings.provider == Provider::Codex);
            let action = agent_chat(
                &mut body_ui,
                AgentPane {
                    id,
                    reset,
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
                Some(ChatAction::UseReset) => self.sessions.use_reset(Provider::Codex),
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
                Some(ChatAction::Command(name)) if name == "mcp" => *run = Some("mcp"),
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

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // a popup or a sheet over the page takes esc for itself first
        if !egui::Popup::is_any_open(&ctx)
            && ctx.memory(|m| m.top_modal_layer().is_none())
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.settings_open = false;
        }
        #[cfg(target_os = "linux")]
        let blur = match &self.blur {
            Some(Err(reason)) => Some(Err(reason.clone())),
            _ => Some(Ok(())),
        };
        // offered until trying it turns out it can't work here, and then the reason stays in its place
        #[cfg(any(windows, target_os = "macos"))]
        let blur = match &self.backdrop {
            _ if !see_through_renderer() => Some(Err(
                "the graphics driver can't draw a see-through window".to_owned(),
            )),
            Some(Err(reason)) => Some(Err(reason.clone())),
            _ => Some(Ok(())),
        };
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
        let blur = None;
        let agents = self.agents(&ctx);
        let mut prefs = self.prefs.clone();
        let distros = self.distros(&ctx);
        let previewed = self.themes.preview.take();
        let mut mcp = self.sessions.mcp.clone();
        let mcp_status = self.sessions.mcp_status.clone();
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
            mcp: mcp.as_mut(),
            mcp_status: &mcp_status,
        }
        .show(ui);
        if mcp != self.sessions.mcp
            && let Some(servers) = mcp.clone()
        {
            self.sessions.mcp = mcp;
            self.sessions
                .outbox
                .push(ClientMsg::SetMcpServers { servers });
        }
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
    fn distros(&mut self, ctx: &egui::Context) -> Option<Vec<String>> {
        self.distros.poll(ctx)
    }

    #[cfg(not(windows))]
    fn distros(&mut self, _ctx: &egui::Context) -> Option<Vec<String>> {
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

// the first run screen: what mux is, the one thing to do next, and whether the agents are ready
fn welcome(ui: &mut egui::Ui, agents: &[(Provider, AgentState)]) -> bool {
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
                        for &(provider, state) in agents {
                            agent_status(ui, provider, state);
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

fn agent_status(ui: &mut egui::Ui, provider: Provider, state: AgentState) {
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
            let (glyph, label, color) = match state {
                AgentState::Ready => (icons::CHECK_CIRCLE, "Installed".to_owned(), p.success),
                AgentState::SignedOut => (
                    icons::WARNING_CIRCLE,
                    format!("Sign in to {}", product(provider)),
                    p.warning,
                ),
                AgentState::Missing => (
                    icons::WARNING_CIRCLE,
                    format!("Install {}", product(provider)),
                    p.warning,
                ),
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

// the name each agent's cli goes by
fn product(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex CLI",
        Provider::Claude => "Claude Code",
        Provider::OpenCode => "OpenCode",
    }
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
// whether an agent can start: its cli is missing, there but signed out, or ready
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Missing,
    SignedOut,
    Ready,
}

impl App {
    // which agents can start, found on PATH at once and marked signed out as the clis answer
    fn agents(&mut self, ctx: &egui::Context) -> [(Provider, AgentState); 3] {
        if self.installed.is_none() {
            let found = installed_agents();
            self.signins = Some(check_signins(ctx, &found));
            self.installed = Some(found);
        }
        let mut agents = self.installed.unwrap_or_else(installed_agents);
        if let Some(answers) = &self.signins {
            for (provider, signed_in) in answers.try_iter() {
                if let Some(agent) = agents.iter_mut().find(|(p, _)| *p == provider)
                    && !signed_in
                {
                    agent.1 = AgentState::SignedOut;
                }
            }
        }
        self.installed = Some(agents);
        agents
    }
}

fn installed_agents() -> [(Provider, AgentState); 3] {
    let state = |found: bool| {
        if found {
            AgentState::Ready
        } else {
            AgentState::Missing
        }
    };
    #[cfg(windows)]
    if let Some(distro) = mux_core::paths::wsl() {
        let found = wsl::installed(&distro, &Provider::ALL.map(cli));
        return Provider::ALL.map(|provider| (provider, state(found.contains(&cli(provider)))));
    }
    Provider::ALL.map(|provider| (provider, state(installed(provider))))
}

// the server runs agents with the same PATH, so finding the cli here means it can start,
// and on windows npm installs them as .cmd files and others as .exe
fn installed(provider: Provider) -> bool {
    let names: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            names.iter().any(|ext| {
                let file = dir.join(cli(provider));
                let file = if ext.is_empty() {
                    file
                } else {
                    file.with_extension(ext)
                };
                file.is_file()
            })
        })
    })
}

// asks claude code and codex whether they're signed in, off the ui thread since each takes a moment to start;
// opencode's free models work without signing in, and an answer that doesn't come counts as signed in
fn check_signins(
    ctx: &egui::Context,
    agents: &[(Provider, AgentState)],
) -> std::sync::mpsc::Receiver<(Provider, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    #[cfg(windows)]
    if mux_core::paths::wsl().is_some() {
        return rx;
    }
    for &(provider, state) in agents {
        let args: &[&str] = match provider {
            Provider::Claude => &["auth", "status"],
            Provider::Codex => &["login", "status"],
            Provider::OpenCode => continue,
        };
        if state != AgentState::Ready || signed_in_by_environment(provider) {
            continue;
        }
        let (tx, ctx) = (tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Some(signed_in) = exits_cleanly(cli(provider), args) {
                if !signed_in {
                    log::info!("{} isn't signed in", provider.label());
                }
                let _ = tx.send((provider, signed_in));
                ctx.request_repaint();
            }
        });
    }
    rx
}

// an api key or a cloud provider set in the environment signs the cli in without a login, which its status
// command may still call logged out, so those people aren't told to sign in when their agents work
fn signed_in_by_environment(provider: Provider) -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    match provider {
        Provider::Claude => [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .into_iter()
        .any(set),
        Provider::Codex => ["OPENAI_API_KEY", "CODEX_API_KEY"].into_iter().any(set),
        Provider::OpenCode => true,
    }
}

// whether a command succeeds, giving up after a while so a stuck cli can't hold the answer forever
fn exits_cleanly(program: &str, args: &[&str]) -> Option<bool> {
    let mut child = mux_core::command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status.success());
        }
        if started.elapsed() > std::time::Duration::from_secs(20) {
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

// how to sign in to an agent from a terminal
pub fn sign_in_command(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude auth login",
        Provider::Codex => "codex login",
        Provider::OpenCode => "opencode auth login",
    }
}

const FOOTER_ROW: f32 = 32.0;

// a quiet row above settings, there only while a newer release is out
impl App {
    // puts the frosted background in or takes it out as transparency is turned on and off, returning whether it's on
    #[cfg(any(windows, target_os = "macos"))]
    fn backdrop(&mut self, frame: &eframe::Frame) -> bool {
        let wanted = self.prefs.transparency != prefs::Transparency::Off && see_through_renderer();
        match (&self.backdrop, wanted) {
            // once it's failed it isn't tried again, and the setting stops being offered
            (Some(Err(_)), _) => false,
            (Some(Ok(on)), wanted) if *on == wanted => wanted,
            (_, true) => match backdrop::apply(frame) {
                Ok(()) => {
                    log::info!("frosted background on");
                    self.backdrop = Some(Ok(true));
                    true
                }
                Err(reason) => {
                    log::info!("no frosted background: {reason}");
                    self.backdrop = Some(Err(reason));
                    false
                }
            },
            (Some(Ok(true)), false) => {
                backdrop::clear(frame);
                self.backdrop = Some(Ok(false));
                false
            }
            (_, false) => false,
        }
    }

    // the update row: available, downloading, ready to restart into, or failed with the release page as the way out
    fn update_row(&mut self, ui: &mut egui::Ui, update: update::Update) {
        let stage = self.installer.as_ref().map(install::Installer::stage);
        let downloadable = update.download.is_some() && install::target().is_ok();
        match stage {
            None => {
                let tip = match (&update.download, install::target()) {
                    (Some(_), Ok(_)) => format!(
                        "{} is out. Click to download and install it.",
                        update.version
                    ),
                    (_, Err(why)) => format!(
                        "{} is out. {why}, so this opens the release page.",
                        update.version
                    ),
                    (None, _) => format!(
                        "{} is out. Open the release page to download it.",
                        update.version
                    ),
                };
                if notice_row(ui, icons::ARROW_CIRCLE_UP, "Update available")
                    .tip(tip)
                    .clicked()
                {
                    match update.download.clone().filter(|_| downloadable) {
                        Some(download) => {
                            self.installer = Some(install::Installer::start(ui.ctx(), download));
                        }
                        None => mux_ui::chat::open_externally(&update.url),
                    }
                }
            }
            Some(install::Stage::Downloading { done, total }) => {
                let label = match (done * 100).checked_div(total) {
                    Some(percent) => format!("Downloading update {}%", percent.min(100)),
                    None => "Downloading update…".to_owned(),
                };
                notice_row(ui, icons::ARROW_CIRCLE_UP, &label)
                    .tip(format!("Getting {}", update.version));
            }
            Some(install::Stage::Ready) => {
                if notice_row(ui, icons::ARROW_CLOCKWISE, "Restart to update")
                    .tip(format!(
                        "{} is installed and starts when Mux does",
                        update.version
                    ))
                    .clicked()
                {
                    install::restart(ui.ctx());
                }
            }
            Some(install::Stage::Failed(why)) => {
                if notice_row(ui, icons::WARNING_CIRCLE, "Update failed")
                    .tip(format!("{why}. Click to open the release page."))
                    .clicked()
                {
                    mux_ui::chat::open_externally(&update.url);
                }
            }
        }
    }
}

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
    resets: usize,
) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), FOOTER_ROW), egui::Sense::click());
    // named for screen readers and test scripts, since the row is painted by hand
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Settings"));
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
        UsageRing::new(windows)
            .provider(provider)
            .resets(resets)
            .show(&mut ring);
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

impl App {
    // agents that finished or need an answer get a dot in the sidebar, and a notification when mux is in the background
    fn alerts(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let looking = self
            .sessions
            .selected_id()
            .filter(|_| focused && !self.settings_open);
        let now = ctx.input(|i| i.time);
        let alerts = self.sessions.take_alerts(looking, now);
        if self.sessions.alert_due() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        for alert in alerts {
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
        #[cfg(any(windows, target_os = "macos"))]
        let blurred = self.backdrop(frame);
        // windows picks light or dark acrylic from what it's told, so it follows mux's own mode
        #[cfg(windows)]
        {
            let dark = ctx.theme() == egui::Theme::Dark;
            if self.told_dark != Some(dark) {
                backdrop::set_dark(frame, dark);
                self.told_dark = Some(dark);
            }
            backdrop::keep_caption_buttons_away(frame, blurred);
        }
        #[cfg(target_os = "macos")]
        backdrop::center_traffic_lights(frame, mux_ui::window::TITLEBAR_HEIGHT);
        #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
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
        #[cfg(windows)]
        renderer_works(&ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let window = ui.max_rect();
                self.titlebar = None;
                self.sidebar.fit(window.width());
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
