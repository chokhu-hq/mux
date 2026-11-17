use std::sync::mpsc::Receiver;

use mux_core::protocol::PrLink;
use mux_ui::{Badge, Button, DiffComment, Segmented, Size, TextInput, Tone};

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
    let header = Rect::from_min_size(panel.min, vec2(panel.width(), FOOTER_ROW + space::SM));
    let mut action = header_row(ui, header, state);
    ui.painter().hline(
        header.x_range(),
        header.bottom() - 0.5,
        egui::Stroke::new(1.0, p.border),
    );
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
    let mut row = ui.new_child(
        UiBuilder::new()
            .id_salt("pr-header")
            .max_rect(rect.shrink2(vec2(space::MD, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row.spacing_mut().item_spacing.x = space::SM;
    if state.number.is_some() {
        if IconButton::new(icons::CARET_LEFT, "All pull requests")
            .show(&mut row)
            .tip("All pull requests")
            .clicked()
        {
            action = Some(PanelAction::Back);
        }
    } else {
        row.label(
            egui::RichText::new(icons::GIT_PULL_REQUEST.to_string())
                .font(font::icon(icon::MD))
                .color(p.text_faint),
        );
    }
    let title = match state.number {
        Some(number) => format!("Pull request #{number}"),
        None => "Pull requests".to_owned(),
    };
    row.label(
        egui::RichText::new(title)
            .font(font::medium(text::MD))
            .color(p.text_muted),
    );
    row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        if IconButton::new(icons::X, "Close")
            .show(ui)
            .tip("Close")
            .clicked()
        {
            action = Some(PanelAction::Close);
        }
        let url = match &state.pr {
            Some(Ok(pr)) => Some(pr.url.clone()),
            _ => None,
        };
        if let Some(url) = url
            && IconButton::new(icons::ARROW_SQUARE_OUT, "Open on GitHub")
                .show(ui)
                .tip("Open on GitHub")
                .clicked()
        {
            action = Some(PanelAction::Url(url));
        }
        let loading = state.loading.is_some() || state.listing.is_some();
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
    action
}

// a quiet line in the middle of the panel, for loading and empty lists
fn notice(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.add_space(space::XL);
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(text)
                .font(font::regular(text::MD))
                .color(color),
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
    let p = theme::palette(ui);
    let mut action = None;
    let rect = ui.max_rect().shrink2(vec2(space::MD, 0.0));
    let mut inner = ui.new_child(UiBuilder::new().id_salt("pr-list").max_rect(rect));
    inner.add_space(space::SM);
    let mut filter = state.filter;
    Segmented::new(&mut filter, &FILTERS).show(&mut inner);
    if filter != state.filter {
        action = Some(PanelAction::Filter(filter));
    }
    inner.add_space(space::SM);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(&mut inner, |ui| match &state.list {
            None => notice(ui, "Reading pull requests…", p.text_faint),
            Some(Err(reason)) => error_strip(ui, reason),
            Some(Ok(list)) if list.is_empty() => {
                let empty = match state.filter {
                    Filter::All => "No open pull requests",
                    Filter::Mine => "You have no open pull requests",
                    Filter::Review => "Nobody is waiting on your review",
                };
                notice(ui, empty, p.text_faint);
            }
            Some(Ok(list)) => {
                for summary in list {
                    if list_row(ui, summary).clicked() {
                        action = Some(PanelAction::Open(summary.number, summary.url.clone()));
                    }
                }
            }
        });
    action
}

fn list_row(ui: &mut egui::Ui, pr: &Summary) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 50.0), egui::Sense::click());
    let label = format!("#{} {}", pr.number, pr.title);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    let hover = mux_ui::hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(theme::radius::LG),
        theme::tint(p, theme::fill::HOVER * hover),
    );
    let inner = rect.shrink2(vec2(space::SM, space::SM));
    // the checks and the review sit at the right, and the title is cut short before them
    let (glyph, color) = checks_mark(p, &pr.checks);
    let mut right = inner.right();
    if let Some(glyph) = glyph {
        ui.painter().text(
            pos2(right - icon::MD / 2.0, inner.top() + 8.0),
            egui::Align2::CENTER_CENTER,
            glyph,
            font::icon(icon::MD),
            color,
        );
        right -= icon::MD + space::SM;
    }
    let (word, tone) = decision(&pr.decision);
    let tag = if pr.draft { Some("Draft") } else { word };
    if let Some(tag) = tag {
        let color = if pr.draft {
            p.text_faint
        } else {
            tone_color(p, tone)
        };
        let galley = ui.painter().text(
            pos2(right, inner.top() + 8.0),
            egui::Align2::RIGHT_CENTER,
            tag,
            font::medium(text::SM),
            color,
        );
        right = galley.left() - space::SM;
    }
    let number = ui.painter().text(
        pos2(inner.left(), inner.top() + 8.0),
        egui::Align2::LEFT_CENTER,
        format!("#{}", pr.number),
        font::medium(text::MD),
        p.text_faint,
    );
    let title = mux_ui::elided(
        ui,
        pr.title.clone(),
        font::medium(text::MD),
        p.text,
        (right - number.right() - space::SM).max(0.0),
    );
    ui.painter().galley(
        pos2(
            number.right() + space::XS,
            inner.top() + 8.0 - title.size().y / 2.0,
        ),
        title,
        p.text,
    );
    let meta = format!(
        "{} · {} · {}",
        pr.author,
        pr.head,
        crate::sessions::sidebar::ago(pr.updated)
    );
    let meta = mux_ui::elided(
        ui,
        meta,
        font::regular(text::SM + 1.0),
        p.text_faint,
        inner.width(),
    );
    ui.painter().galley(
        pos2(inner.left(), inner.bottom() - 7.0 - meta.size().y / 2.0),
        meta,
        p.text_faint,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// one mark for all of a pull request's checks: failed if any failed, else pending if any are, else passed
fn checks_mark(p: &theme::Palette, checks: &[Check]) -> (Option<char>, egui::Color32) {
    let any = |state| checks.iter().any(|c| c.state == state);
    if checks.is_empty() {
        (None, p.text_faint)
    } else if any(CheckState::Failed) {
        (Some(icons::X_CIRCLE), p.danger)
    } else if any(CheckState::Pending) {
        (Some(icons::CIRCLE), p.warning)
    } else {
        (Some(icons::CHECK_CIRCLE), p.success)
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

fn tone_color(p: &theme::Palette, tone: Tone) -> egui::Color32 {
    match tone {
        Tone::Success => p.success,
        Tone::Danger => p.danger,
        Tone::Warning => p.warning,
        Tone::Accent => p.accent,
        _ => p.text_muted,
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
            notice(&mut inner, "Reading the pull request…", p.text_faint);
            return None;
        }
    };
    // the review box stays at the bottom while the rest scrolls
    let composer_height = 128.0;
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
    ui.label(
        egui::RichText::new(&pr.title)
            .font(font::semibold(text::LG))
            .color(p.text),
    );
    ui.add_space(space::SM);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        state_badge(pr).show(ui);
        if let (Some(word), tone) = decision(&pr.decision) {
            Badge::new(word).tone(tone).show(ui);
        }
        if pr.mergeable == "CONFLICTING" {
            Badge::new("Conflicts").tone(Tone::Warning).show(ui);
        }
        for label in &pr.labels {
            Badge::new(label).show(ui);
        }
    });
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.label(
            egui::RichText::new(format!("{} → {}", pr.head, pr.base))
                .font(font::regular(text::SM + 1.0))
                .color(p.text_muted),
        );
        ui.label(
            egui::RichText::new(format!("+{}", pr.additions))
                .font(font::medium(text::SM + 1.0))
                .color(p.success),
        );
        ui.label(
            egui::RichText::new(format!("−{}", pr.deletions))
                .font(font::medium(text::SM + 1.0))
                .color(p.danger),
        );
        ui.label(
            egui::RichText::new(format!(
                "{} {}",
                pr.files,
                if pr.files == 1 { "file" } else { "files" }
            ))
            .font(font::regular(text::SM + 1.0))
            .color(p.text_faint),
        );
    });
    ui.add_space(space::SM);
    let open = pr.state == "OPEN";
    let busy = state.writing.is_some();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        if open
            && pr.draft
            && Button::new("Mark ready")
                .size(Size::Sm)
                .enabled(!busy)
                .show(ui)
                .tip("Mark it ready for review")
                .clicked()
        {
            action = Some(PanelAction::Write(Write::Ready));
        }
        if open && !pr.draft {
            let merge = Button::new("Merge")
                .size(Size::Sm)
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
                .size(Size::Sm)
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
        caption(ui, "Checks");
        for check in &pr.checks {
            if let Some(url) = check_row(ui, check) {
                action = Some(PanelAction::Url(url));
            }
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
                        send = Button::new("Send to agent")
                            .size(Size::Sm)
                            .ghost()
                            .show(ui)
                            .tip("Add it to the next message, the way diff comments go")
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
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        if Button::new("Comment")
            .size(Size::Sm)
            .primary()
            .enabled(!busy && !body.is_empty())
            .show(ui)
            .clicked()
        {
            action = Some(PanelAction::Write(Write::Comment(body.clone())));
        }
        let open = pr.state == "OPEN";
        if Button::new("Approve")
            .size(Size::Sm)
            .enabled(!busy && open)
            .show(ui)
            .clicked()
        {
            action = Some(PanelAction::Write(Write::Approve(body.clone())));
        }
        if Button::new("Request changes")
            .size(Size::Sm)
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
