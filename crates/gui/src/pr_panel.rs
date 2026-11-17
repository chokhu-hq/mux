use std::sync::mpsc::Receiver;

use mux_core::protocol::PrLink;
use mux_ui::{Badge, Button, DiffComment, EmptyState, Segmented, TextInput, Tone};

use super::*;
use crate::pr::{self, Check, CheckState, Filter, Inline, Merge, Note, Pr, Summary, Write};

const FILTERS: [(Filter, &str); 3] = [
    (Filter::All, "All"),
    (Filter::Mine, "Mine"),
    (Filter::Review, "Review requested"),
];

// the pull requests panel on the right: the project's open ones, or one of them in full
pub(crate) struct PrPanel {
    // the session it was opened from, which picked pull requests are linked to and comments are sent to
    pub session: u64,
    group: u64,
    // where gh runs, none when the project has no folder
    dir: Option<String>,
    filter: Filter,
    list: Option<Result<Vec<Summary>, String>>,
    listing: Option<Receiver<Result<Vec<Summary>, String>>>,
    // the pull request shown in full, when one is
    number: Option<u64>,
    pr: Option<Result<Pr, String>>,
    loading: Option<Receiver<Result<Pr, String>>>,
    writing: Option<Receiver<Result<(), String>>>,
    // why the last change to github didn't go through
    error: Option<String>,
    draft: String,
}

enum PanelAction {
    Close,
    Back,
    Refresh,
    Filter(Filter),
    Open(u64, String),
    Checkout,
    Write(Write),
    Send(DiffComment),
    Url(String),
}

impl PrPanel {
    fn new(session: u64, group: u64, dir: Option<String>) -> Self {
        Self {
            session,
            group,
            dir,
            filter: Filter::All,
            list: None,
            listing: None,
            number: None,
            pr: None,
            loading: None,
            writing: None,
            error: None,
            draft: String::new(),
        }
    }

    fn dir(&self) -> Result<String, String> {
        self.dir.clone().ok_or_else(|| {
            "This project has no folder yet, so there's nowhere to run gh.".to_owned()
        })
    }

    fn fetch_list(&mut self, ctx: &egui::Context) {
        let (dir, filter) = (self.dir(), self.filter);
        self.listing = Some(crate::in_background(ctx, move || pr::list(&dir?, filter)));
    }

    fn fetch_pr(&mut self, ctx: &egui::Context) {
        let Some(number) = self.number else {
            return;
        };
        let dir = self.dir();
        self.loading = Some(crate::in_background(ctx, move || pr::view(&dir?, number)));
    }

    fn write(&mut self, ctx: &egui::Context, action: Write) {
        let (Some(number), None) = (self.number, &self.writing) else {
            return;
        };
        self.error = None;
        let dir = self.dir();
        self.writing = Some(crate::in_background(ctx, move || {
            pr::write(&dir?, number, &action)
        }));
    }

    // answers from gh that came in since last frame, with a finished change read back from github
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(list) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.listing = None;
            self.list = Some(list);
        }
        if let Some(pr) = self.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.loading = None;
            self.pr = Some(pr);
        }
        if let Some(done) = self.writing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.writing = None;
            match done {
                Ok(()) => {
                    self.draft.clear();
                    self.fetch_pr(ctx);
                }
                Err(reason) => self.error = Some(reason),
            }
        }
    }
}

impl App {
    // the selected session's project's pull requests, or the one linked to it
    pub(crate) fn toggle_prs(&mut self, ctx: &egui::Context) {
        if self.pr_open {
            self.pr_open = false;
            return;
        }
        let Some(id) = self.sessions.selected_id() else {
            return;
        };
        match self.sessions.info(id).and_then(|s| s.pr.clone()) {
            Some(link) => self.show_pr(ctx, id, link.number),
            None => self.show_prs(ctx, id),
        }
    }

    pub(crate) fn show_prs(&mut self, ctx: &egui::Context, session: u64) {
        let Some((group, dir)) = self.sessions.checkout(session) else {
            return;
        };
        let mut panel = PrPanel::new(session, group, dir);
        panel.fetch_list(ctx);
        self.pr = Some(panel);
        self.pr_open = true;
        self.viewer_open = false;
        self.files_open = false;
    }

    pub(crate) fn show_pr(&mut self, ctx: &egui::Context, session: u64, number: u64) {
        let Some((group, dir)) = self.sessions.checkout(session) else {
            return;
        };
        let mut panel = PrPanel::new(session, group, dir);
        panel.number = Some(number);
        panel.fetch_pr(ctx);
        self.pr = Some(panel);
        self.pr_open = true;
        self.viewer_open = false;
        self.files_open = false;
    }

    // the pull requests panel, returning what's left of `main` for the session
    pub(crate) fn pr_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        let open = self.pr_open && self.pr.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("pr-panel"), open, Spring::SMOOTH)
            .clamp(0.0, 1.0);
        if t <= 0.001 {
            if !open {
                self.pr = None;
            }
            return main;
        }
        // the diff panel may still be sliding out beside it, so the chat keeps some room either way
        let full = self
            .pr_width
            .clamp(360.0, (main.width() * 0.7).max(360.0))
            .min((main.width() - 320.0).max(0.0));
        let left = main.right() - full * t;
        let edge = if open { main.right() - full } else { left };
        let handle = Rect::from_min_max(
            pos2(left - 3.0, main.top()),
            pos2(left + 3.0, main.bottom()),
        );
        let drag = ui
            .interact(
                handle,
                egui::Id::unique("pr-panel-resize"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if drag.dragged() {
            self.pr_width = full - drag.drag_delta().x;
        }
        let panel = Rect::from_min_size(pos2(left, main.top()), vec2(full, main.height()));
        let mut pane = ui.new_child(UiBuilder::new().id_salt("pr-panel").max_rect(panel));
        pane.set_clip_rect(panel.intersect(main).intersect(ui.clip_rect()));
        let Some(state) = &mut self.pr else {
            return main;
        };
        state.poll(&ctx);
        let action = draw(&mut pane, state);
        match action {
            Some(PanelAction::Close) => self.pr_open = false,
            Some(PanelAction::Back) => {
                state.number = None;
                state.pr = None;
                state.error = None;
                if state.list.is_none() {
                    state.fetch_list(&ctx);
                }
            }
            Some(PanelAction::Refresh) => match state.number {
                Some(_) => state.fetch_pr(&ctx),
                None => state.fetch_list(&ctx),
            },
            Some(PanelAction::Filter(filter)) => {
                state.filter = filter;
                state.list = None;
                state.fetch_list(&ctx);
            }
            Some(PanelAction::Open(number, url)) => {
                state.number = Some(number);
                state.pr = None;
                state.fetch_pr(&ctx);
                self.sessions.outbox.push(ClientMsg::SetPr {
                    id: state.session,
                    pr: Some(PrLink { number, url }),
                });
            }
            Some(PanelAction::Checkout) => {
                if let Some(Ok(pr)) = &state.pr {
                    let link = PrLink {
                        number: pr.number,
                        url: pr.url.clone(),
                    };
                    let title = format!("#{} {}", pr.number, pr.title);
                    self.sessions.checkout_pr(state.group, link, title);
                }
            }
            Some(PanelAction::Write(write)) => state.write(&ctx, write),
            Some(PanelAction::Send(comment)) => {
                let session = state.session;
                self.sessions.local(session).comments.push(comment);
            }
            Some(PanelAction::Url(url)) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            None => {}
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }
}

fn draw(ui: &mut egui::Ui, state: &mut PrPanel) -> Option<PanelAction> {
    let p = theme::palette(ui);
    let panel = ui.max_rect();
    ui.painter().rect_filled(panel, 0.0, p.bg);
    ui.painter().vline(
        panel.left() + 0.5,
        panel.y_range(),
        egui::Stroke::new(1.0, p.border),
    );
    let header = Rect::from_min_size(panel.min, vec2(panel.width(), theme::control::HEADER));
    let mut action = header_row(ui, header, state);
    let body = Rect::from_min_max(pos2(panel.left() + 1.0, header.bottom()), panel.max);
    let mut body_ui = ui.new_child(UiBuilder::new().id_salt("pr-body").max_rect(body));
    body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
    let shown = match state.number {
        Some(_) => detail(&mut body_ui, state),
        None => list(&mut body_ui, state),
    };
    action = action.or(shown);
    action
}

fn header_row(ui: &mut egui::Ui, rect: Rect, state: &PrPanel) -> Option<PanelAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let url = match &state.pr {
        Some(Ok(pr)) => Some(pr.url.clone()),
        _ => None,
    };
    let loading = state.loading.is_some() || state.listing.is_some();
    let mut row = panel_header(ui, rect, "pr-header", |ui| {
        if IconButton::new(icons::X, "Close")
            .show(ui)
            .tip("Close")
            .clicked()
        {
            action = Some(PanelAction::Close);
        }
        if let Some(url) = url
            && IconButton::new(icons::ARROW_SQUARE_OUT, "Open on GitHub")
                .show(ui)
                .tip("Open on GitHub")
                .clicked()
        {
            action = Some(PanelAction::Url(url));
        }
        if IconButton::new(icons::ARROW_CLOCKWISE, "Refresh")
            .show(ui)
            .tip(if loading {
                "Reading from GitHub…"
            } else {
                "Refresh"
            })
            .clicked()
            && !loading
        {
            action = Some(PanelAction::Refresh);
        }
    });
    match state.number {
        Some(number) => {
            if IconButton::new(icons::CARET_LEFT, "All pull requests")
                .show(&mut row)
                .tip("All pull requests")
                .clicked()
            {
                action = Some(PanelAction::Back);
            }
            row.label(
                egui::RichText::new(format!("Pull request #{number}"))
                    .font(font::medium(text::MD))
                    .color(p.text),
            );
        }
        None => panel_title(&mut row, icons::GIT_PULL_REQUEST, "Pull requests"),
    }
    action
}

// a spinner over a quiet line in the middle of the panel, while gh answers
fn loading(ui: &mut egui::Ui, label: &str) {
    let p = theme::palette(ui);
    ui.add_space(space::XL * 2.0);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), icon::LG), egui::Sense::hover());
    mux_ui::chat::spinner(ui, rect.center(), icon::LG, p.text_muted);
    ui.add_space(space::SM);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(label)
                .font(font::regular(text::MD))
                .color(p.text_faint),
        );
    });
}

fn error_strip(ui: &mut egui::Ui, reason: &str) {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.danger.gamma_multiply(0.12))
        .corner_radius(egui::CornerRadius::same(theme::radius::MD))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(reason)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.danger),
            );
        });
}

fn list(ui: &mut egui::Ui, state: &mut PrPanel) -> Option<PanelAction> {
    let mut action = None;
    let rect = ui.max_rect().shrink2(vec2(space::SM, 0.0));
    let mut inner = ui.new_child(UiBuilder::new().id_salt("pr-list").max_rect(rect));
    inner.add_space(space::MD);
    let mut filter = state.filter;
    inner.horizontal(|ui| {
        ui.add_space(space::XS);
        Segmented::new(&mut filter, &FILTERS).fit().show(ui);
    });
    if filter != state.filter {
        action = Some(PanelAction::Filter(filter));
    }
    inner.add_space(space::SM);
    match &state.list {
        None => loading(&mut inner, "Reading pull requests…"),
        Some(Err(reason)) => {
            inner.add_space(space::XS);
            error_strip(&mut inner, reason);
        }
        Some(Ok(list)) if list.is_empty() => {
            let (title, detail) = match state.filter {
                Filter::All => (
                    "No open pull requests",
                    "Push a worktree's branch to open one.",
                ),
                Filter::Mine => (
                    "Nothing of yours is open",
                    "Pull requests you open show up here.",
                ),
                Filter::Review => (
                    "Nobody is waiting on you",
                    "Pull requests that ask for your review show up here.",
                ),
            };
            EmptyState::new(icons::GIT_PULL_REQUEST, title)
                .detail(detail)
                .show(&mut inner);
        }
        Some(Ok(list)) => {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(&mut inner, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for summary in list {
                        if list_row(ui, summary).clicked() {
                            action = Some(PanelAction::Open(summary.number, summary.url.clone()));
                        }
                    }
                });
        }
    }
    action
}

// a pull request as two lines: its title with its review status, then who, where and when with its checks
fn list_row(ui: &mut egui::Ui, pr: &Summary) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 54.0), egui::Sense::click());
    let label = format!("#{} {}", pr.number, pr.title);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    let hover = mux_ui::hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(theme::radius::LG),
        theme::tint(p, theme::fill::HOVER * hover),
    );
    let inner = rect.shrink2(vec2(space::MD, space::SM));
    let (top, bottom) = (inner.top() + 9.0, inner.bottom() - 8.0);
    let mut right = inner.right();
    if let Some((word, tone)) = status(pr) {
        let badge = Badge::new(word).tone(tone).paint(ui, pos2(right, top));
        right = badge.left() - space::SM;
    }
    let title = mux_ui::elided(
        ui,
        pr.title.clone(),
        font::medium(text::MD),
        p.text,
        (right - inner.left()).max(0.0),
    );
    ui.painter().galley(
        pos2(inner.left(), top - title.size().y / 2.0),
        title,
        p.text,
    );
    let mut meta_right = inner.right();
    if let Some((color, summary)) = checks_mark(p, &pr.checks) {
        let dot = pos2(inner.right() - 4.0, bottom);
        ui.painter().circle_filled(dot, 3.5, color);
        ui.interact(
            egui::Rect::from_center_size(dot, vec2(14.0, 14.0)),
            response.id.with("checks"),
            egui::Sense::hover(),
        )
        .tip(summary);
        meta_right = dot.x - space::MD;
    }
    let meta = format!(
        "#{} · {} · {} · {}",
        pr.number,
        pr.author,
        pr.head,
        crate::sessions::sidebar::ago(pr.updated)
    );
    let meta = mux_ui::elided(
        ui,
        meta,
        font::regular(text::SM + 1.0),
        p.text_faint,
        (meta_right - inner.left()).max(0.0),
    );
    ui.painter().galley(
        pos2(inner.left(), bottom - meta.size().y / 2.0),
        meta,
        p.text_faint,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// the one status a row shows: a draft says so, otherwise where its review stands
fn status(pr: &Summary) -> Option<(&'static str, Tone)> {
    if pr.draft {
        return Some(("Draft", Tone::Neutral));
    }
    match decision(&pr.decision) {
        (Some(word), tone) => Some((word, tone)),
        (None, _) => None,
    }
}

// one colour for all of a pull request's checks: failed if any failed, else running if any are, else passed
fn checks_mark(p: &theme::Palette, checks: &[Check]) -> Option<(egui::Color32, &'static str)> {
    let any = |state| checks.iter().any(|c| c.state == state);
    if checks.is_empty() {
        None
    } else if any(CheckState::Failed) {
        Some((p.danger, "Some checks failed"))
    } else if any(CheckState::Pending) {
        Some((p.warning, "Checks are running"))
    } else {
        Some((p.success, "All checks passed"))
    }
}

fn check_mark(p: &theme::Palette, state: CheckState) -> (char, egui::Color32) {
    match state {
        CheckState::Passed => (icons::CHECK_CIRCLE, p.success),
        CheckState::Failed => (icons::X_CIRCLE, p.danger),
        CheckState::Pending => (icons::CIRCLE, p.warning),
        CheckState::Skipped => (icons::CIRCLE, p.text_faint),
    }
}

fn decision(decision: &str) -> (Option<&'static str>, Tone) {
    match decision {
        "APPROVED" => (Some("Approved"), Tone::Success),
        "CHANGES_REQUESTED" => (Some("Changes requested"), Tone::Danger),
        "REVIEW_REQUIRED" => (Some("Review required"), Tone::Warning),
        _ => (None, Tone::Neutral),
    }
}

fn state_badge(pr: &Pr) -> Badge {
    match (pr.state.as_str(), pr.draft) {
        ("MERGED", _) => Badge::new("Merged").tone(Tone::Accent),
        ("CLOSED", _) => Badge::new("Closed").tone(Tone::Danger),
        (_, true) => Badge::new("Draft"),
        _ => Badge::new("Open").tone(Tone::Success),
    }
}

fn caption(ui: &mut egui::Ui, label: &str) {
    let p = theme::palette(ui);
    ui.add_space(space::MD);
    ui.label(
        egui::RichText::new(label)
            .font(font::medium(text::SM))
            .color(p.text_faint),
    );
    ui.add_space(space::XS);
}

fn detail(ui: &mut egui::Ui, state: &mut PrPanel) -> Option<PanelAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let rect = ui.max_rect();
    let pr = match &state.pr {
        Some(Ok(pr)) => pr.clone(),
        Some(Err(reason)) => {
            let mut inner = ui.new_child(UiBuilder::new().max_rect(rect.shrink(space::MD)));
            error_strip(&mut inner, reason);
            return None;
        }
        None => {
            let mut inner = ui.new_child(UiBuilder::new().max_rect(rect));
            loading(&mut inner, "Reading the pull request…");
            return None;
        }
    };
    // the review box stays at the bottom while the rest scrolls
    let composer_height = 140.0;
    let split = rect.bottom() - composer_height;
    let top = Rect::from_min_max(rect.min, pos2(rect.right(), split));
    let bottom = Rect::from_min_max(pos2(rect.left(), split), rect.max);
    let mut scroll_ui = ui.new_child(UiBuilder::new().id_salt("pr-detail").max_rect(top));
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(&mut scroll_ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(space::MD as i8, space::MD as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    action = overview(ui, state, &pr);
                });
        });
    ui.painter().hline(
        bottom.x_range(),
        bottom.top() + 0.5,
        egui::Stroke::new(1.0, p.border),
    );
    let mut composer = ui.new_child(
        UiBuilder::new()
            .id_salt("pr-composer")
            .max_rect(bottom.shrink(space::MD)),
    );
    action.or(review_box(&mut composer, state, &pr))
}

fn overview(ui: &mut egui::Ui, state: &PrPanel, pr: &Pr) -> Option<PanelAction> {
    let p = theme::palette(ui);
    let mut action = None;
    ui.add(
        egui::Label::new(
            egui::RichText::new(&pr.title)
                .font(font::semibold(text::LG))
                .color(p.text),
        )
        .wrap(),
    );
    ui.add_space(space::SM);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        state_badge(pr).show(ui);
        let quiet = |text: String, color| {
            egui::RichText::new(text)
                .font(font::regular(text::SM + 1.0))
                .color(color)
        };
        ui.label(quiet(format!("#{}", pr.number), p.text_faint));
        ui.label(quiet(format!("{} → {}", pr.head, pr.base), p.text_muted));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            let count = |text: String, color| {
                egui::RichText::new(text)
                    .font(font::medium(text::SM + 1.0))
                    .color(color)
            };
            ui.label(count(format!("+{}", pr.additions), p.success));
            ui.label(count(format!("−{}", pr.deletions), p.danger));
        });
        let files = if pr.files == 1 { "file" } else { "files" };
        ui.label(quiet(format!("{} {files}", pr.files), p.text_faint));
    });
    let decided = decision(&pr.decision).0.is_some();
    let conflicts = pr.mergeable == "CONFLICTING";
    if decided || conflicts || !pr.labels.is_empty() {
        ui.add_space(space::SM);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            if let (Some(word), tone) = decision(&pr.decision) {
                Badge::new(word).tone(tone).show(ui);
            }
            if conflicts {
                Badge::new("Conflicts").tone(Tone::Warning).show(ui);
            }
            for label in &pr.labels {
                Badge::new(label).show(ui);
            }
        });
    }
    ui.add_space(space::MD);
    let open = pr.state == "OPEN";
    let busy = state.writing.is_some();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        if open
            && pr.draft
            && Button::new("Mark ready")
                .primary()
                .enabled(!busy)
                .show(ui)
                .tip("Mark it ready for review")
                .clicked()
        {
            action = Some(PanelAction::Write(Write::Ready));
        }
        if open && !pr.draft {
            let merge = Button::new("Merge")
                .primary()
                .menu()
                .enabled(!busy)
                .show(ui);
            egui::Popup::menu(&merge)
                .align(egui::RectAlign::BOTTOM_START)
                .gap(space::XS)
                .show(|ui| {
                    ui.set_width(300.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (how, label, detail) in Merge::ALL {
                        if MenuItem::new(label).detail(detail).show(ui).clicked() {
                            action = Some(PanelAction::Write(Write::Merge(how)));
                            ui.close();
                        }
                    }
                });
        }
        if open
            && Button::new("Check out")
                .show(ui)
                .tip("Start an agent in a new worktree on this branch")
                .clicked()
        {
            action = Some(PanelAction::Checkout);
        }
        if busy {
            ui.label(
                egui::RichText::new("Updating GitHub…")
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        }
    });
    if let Some(reason) = &state.error {
        ui.add_space(space::SM);
        error_strip(ui, reason);
    }
    if !pr.body.trim().is_empty() {
        ui.add_space(space::MD);
        mux_ui::chat::markdown(ui, &pr.body);
    }
    if !pr.checks.is_empty() {
        ui.add_space(space::MD);
        if let Some(url) = checks_card(ui, &pr.checks) {
            action = Some(PanelAction::Url(url));
        }
    }
    let mut notes: Vec<(&Note, bool)> = pr
        .reviews
        .iter()
        .map(|r| (r, true))
        .chain(pr.comments.iter().map(|c| (c, false)))
        .collect();
    notes.sort_by_key(|(note, _)| note.when);
    if !notes.is_empty() {
        caption(ui, "Reviews and comments");
        for (i, (note, review)) in notes.into_iter().enumerate() {
            if note_card(ui, ("note", i), note, review, None) {
                action = Some(PanelAction::Send(note_comment(pr.number, note, review)));
            }
        }
    }
    if !pr.inline.is_empty() {
        caption(ui, "Inline comments");
        for (i, inline) in pr.inline.iter().enumerate() {
            let note = Note {
                author: inline.author.clone(),
                when: inline.when,
                body: inline.body.clone(),
                state: String::new(),
            };
            let place = inline.place();
            if note_card(ui, ("inline", i), &note, false, Some(&place)) {
                action = Some(PanelAction::Send(inline_comment(
                    state.dir.as_deref(),
                    inline,
                )));
            }
        }
    }
    action
}

// the checks in one card: a summary line, then the ones that failed or are still running,
// and the passed ones folded into a single row that opens to list them
fn checks_card(ui: &mut egui::Ui, checks: &[Check]) -> Option<String> {
    let p = theme::palette(ui);
    let mut clicked = None;
    let count = |state| checks.iter().filter(|c| c.state == state).count();
    let (failed, running, passed) = (
        count(CheckState::Failed),
        count(CheckState::Pending),
        count(CheckState::Passed),
    );
    let mut summary = Vec::new();
    if failed > 0 {
        summary.push(format!("{failed} failed"));
    }
    if running > 0 {
        summary.push(format!("{running} running"));
    }
    if passed > 0 {
        summary.push(format!("{passed} passed"));
    }
    let mut rest: Vec<&Check> = checks
        .iter()
        .filter(|c| c.state != CheckState::Passed)
        .collect();
    rest.sort_by_key(|c| match c.state {
        CheckState::Failed => 0,
        CheckState::Pending => 1,
        _ => 2,
    });
    let open_id = ui.make_persistent_id("passed-checks");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    theme::card(ui)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = space::XS;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Checks")
                        .font(font::medium(text::MD))
                        .color(p.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(summary.join(" · "))
                            .font(font::regular(text::SM + 1.0))
                            .color(p.text_faint),
                    );
                });
            });
            ui.add_space(space::XS);
            for check in rest {
                if let Some(url) = check_row(ui, check) {
                    clicked = Some(url);
                }
            }
            if passed > 0 {
                let row = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::SM;
                        ui.label(
                            egui::RichText::new(icons::CHECK_CIRCLE.to_string())
                                .font(font::icon(icon::MD))
                                .color(p.success),
                        );
                        let word = if passed == 1 { "check" } else { "checks" };
                        ui.label(
                            egui::RichText::new(format!("{passed} {word} passed"))
                                .font(font::medium(text::SM + 1.0))
                                .color(p.text_muted),
                        );
                        ui.label(
                            egui::RichText::new(
                                if open {
                                    icons::CARET_DOWN
                                } else {
                                    icons::CARET_RIGHT
                                }
                                .to_string(),
                            )
                            .font(font::icon(icon::SM))
                            .color(p.text_faint),
                        );
                    })
                    .response;
                let row = ui
                    .interact(row.rect, open_id.with("row"), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                row.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, "Passed checks")
                });
                if row.clicked() {
                    open = !open;
                }
                if open {
                    for check in checks.iter().filter(|c| c.state == CheckState::Passed) {
                        ui.horizontal(|ui| {
                            ui.add_space(icon::MD + space::SM);
                            if let Some(url) = check_row(ui, check) {
                                clicked = Some(url);
                            }
                        });
                    }
                }
            }
        });
    ui.data_mut(|d| d.insert_temp(open_id, open));
    clicked
}

// returns the check's page when its name is clicked
fn check_row(ui: &mut egui::Ui, check: &Check) -> Option<String> {
    let p = theme::palette(ui);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        let (glyph, color) = check_mark(p, check.state);
        ui.label(
            egui::RichText::new(glyph.to_string())
                .font(font::icon(icon::MD))
                .color(color),
        );
        let name = if check.workflow.is_empty() {
            check.name.clone()
        } else {
            format!("{} / {}", check.workflow, check.name)
        };
        let label = egui::RichText::new(name)
            .font(font::medium(text::SM + 1.0))
            .color(p.text);
        match &check.url {
            Some(url) => {
                let link = ui
                    .add(egui::Label::new(label).sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .tip("Open the check's page");
                if link.clicked() {
                    clicked = Some(url.clone());
                }
            }
            None => {
                ui.label(label);
            }
        }
        ui.label(
            egui::RichText::new(&check.detail)
                .font(font::regular(text::SM + 1.0))
                .color(p.text_faint),
        );
    });
    clicked
}

fn review_state(state: &str) -> Option<(&'static str, Tone)> {
    match state {
        "APPROVED" => Some(("Approved", Tone::Success)),
        "CHANGES_REQUESTED" => Some(("Changes requested", Tone::Danger)),
        "DISMISSED" => Some(("Dismissed", Tone::Neutral)),
        "COMMENTED" => Some(("Reviewed", Tone::Neutral)),
        _ => None,
    }
}

// a review, comment or inline comment as a card, returning true when it's sent to the agent
fn note_card(
    ui: &mut egui::Ui,
    salt: (&str, usize),
    note: &Note,
    review: bool,
    place: Option<&str>,
) -> bool {
    let p = theme::palette(ui);
    let mut send = false;
    ui.push_id(salt, |ui| {
        theme::card(ui)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(place) = place {
                    ui.label(
                        egui::RichText::new(place)
                            .font(font::mono(text::MONO_SM))
                            .color(p.text_muted),
                    );
                    ui.add_space(space::XS);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::SM;
                    avatar(ui, &note.author);
                    ui.label(
                        egui::RichText::new(&note.author)
                            .font(font::medium(text::MD))
                            .color(p.text),
                    );
                    if let Some((word, tone)) = review.then(|| review_state(&note.state)).flatten()
                    {
                        Badge::new(word).tone(tone).show(ui);
                    }
                    let when = crate::sessions::sidebar::ago(note.when);
                    if !when.is_empty() {
                        ui.label(
                            egui::RichText::new(when)
                                .font(font::regular(text::SM + 1.0))
                                .color(p.text_faint),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        send = IconButton::new(icons::PAPER_PLANE_RIGHT, "Send to agent")
                            .show(ui)
                            .tip("Send to the agent with the next message")
                            .clicked();
                    });
                });
                if !note.body.trim().is_empty() {
                    ui.add_space(space::XS);
                    mux_ui::chat::markdown(ui, &note.body);
                }
            });
    });
    ui.add_space(space::SM);
    send
}

// a circle with the first letter of someone's name, coloured by the name so each person keeps one
fn avatar(ui: &mut egui::Ui, name: &str) {
    let p = theme::palette(ui);
    let colors = [p.accent, p.agent, p.shell, p.task, p.warning, p.success];
    let pick = name.bytes().map(usize::from).sum::<usize>() % colors.len();
    let color = colors[pick];
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 11.0, color.gamma_multiply(0.2));
    let initial = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        font::semibold(text::SM),
        color,
    );
}

fn note_comment(number: u64, note: &Note, review: bool) -> DiffComment {
    let kind = if review { "review" } else { "comment" };
    let verdict = match note.state.as_str() {
        "APPROVED" => ", approved",
        "CHANGES_REQUESTED" => ", changes requested",
        _ => "",
    };
    DiffComment {
        path: String::new(),
        file: format!("PR #{number}"),
        range: format!("{kind} by @{}{verdict}", note.author),
        old: None,
        new: None,
        text: note.body.trim().to_owned(),
        diff: String::new(),
    }
}

fn inline_comment(dir: Option<&str>, inline: &Inline) -> DiffComment {
    let old_side = inline.side == "LEFT";
    let sign = if old_side { '-' } else { '+' };
    let lines = inline
        .line
        .map(|end| (inline.start.unwrap_or(end).min(end), end));
    let range = match lines {
        Some((start, end)) if start != end => format!("{sign}{start} to {sign}{end}"),
        Some((_, end)) => format!("{sign}{end}"),
        None => "the file".to_owned(),
    };
    let path = match dir {
        Some(dir) => mux_core::paths::local(&mux_core::paths::join(dir, &inline.path)),
        None => inline.path.clone(),
    };
    DiffComment {
        path,
        file: inline.path.clone(),
        range,
        old: lines.filter(|_| old_side),
        new: lines.filter(|_| !old_side),
        text: format!("@{}: {}", inline.author, inline.body.trim()),
        diff: inline.hunk.clone(),
    }
}

fn review_box(ui: &mut egui::Ui, state: &mut PrPanel, pr: &Pr) -> Option<PanelAction> {
    let mut action = None;
    let busy = state.writing.is_some();
    TextInput::new(&mut state.draft)
        .placeholder("Leave a comment or a review")
        .multiline(3)
        .show(ui);
    ui.add_space(space::SM);
    let body = state.draft.trim().to_owned();
    let open = pr.state == "OPEN";
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        if Button::new("Comment")
            .primary()
            .enabled(!busy && !body.is_empty())
            .show(ui)
            .clicked()
        {
            action = Some(PanelAction::Write(Write::Comment(body.clone())));
        }
        if Button::new("Approve")
            .enabled(!busy && open)
            .show(ui)
            .clicked()
        {
            action = Some(PanelAction::Write(Write::Approve(body.clone())));
        }
        if Button::new("Request changes")
            .enabled(!busy && open && !body.is_empty())
            .show(ui)
            .tip("Ask for changes, with what's written above")
            .clicked()
        {
            action = Some(PanelAction::Write(Write::RequestChanges(body.clone())));
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(side: &str, start: Option<usize>, line: Option<usize>) -> Inline {
        Inline {
            author: "ana".to_owned(),
            when: 0,
            body: "Rename this\n".to_owned(),
            path: "src/a.rs".to_owned(),
            line,
            start,
            side: side.to_owned(),
            hunk: "@@ -1 +1 @@\n+x".to_owned(),
            reply: false,
        }
    }

    #[test]
    fn inline_comments_become_diff_comments() {
        let c = inline_comment(Some("/p"), &inline("RIGHT", Some(10), Some(12)));
        assert_eq!(c.range, "+10 to +12");
        assert_eq!(c.new, Some((10, 12)));
        assert_eq!(c.old, None);
        assert_eq!(c.file, "src/a.rs");
        assert_eq!(c.path, "/p/src/a.rs");
        assert_eq!(c.text, "@ana: Rename this");
        let c = inline_comment(None, &inline("LEFT", None, Some(4)));
        assert_eq!((c.range.as_str(), c.old), ("-4", Some((4, 4))));
    }

    #[test]
    fn reviews_become_comments_on_the_pull_request() {
        let note = Note {
            author: "bo".to_owned(),
            when: 0,
            body: " Needs a test ".to_owned(),
            state: "CHANGES_REQUESTED".to_owned(),
        };
        let c = note_comment(9, &note, true);
        assert_eq!(c.file, "PR #9");
        assert_eq!(c.range, "review by @bo, changes requested");
        assert_eq!(c.text, "Needs a test");
        assert!(c.diff.is_empty());
    }
}
