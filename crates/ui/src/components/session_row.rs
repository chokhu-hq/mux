use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Response, Sense, Ui, Vec2,
    WidgetInfo, pos2,
};
use mux_core::{Provider, SessionKind, SessionStatus};

use super::{hover_t, provider_color, provider_mark};
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, Palette, font, icon, radius, space, text};

pub struct SessionRow<'a> {
    title: &'a str,
    kind: SessionKind,
    status: SessionStatus,
    context: &'a str,
    time: &'a str,
    branch: &'a str,
    provider: Option<Provider>,
    selected: bool,
    unread: bool,
    model: &'a str,
    // lines added and removed by an agent, shown at the end of the bottom line
    changes: Option<(u32, u32)>,
}

impl<'a> SessionRow<'a> {
    pub const HEIGHT: f32 = 70.0;

    pub fn new(title: &'a str, kind: SessionKind, status: SessionStatus) -> Self {
        Self {
            title,
            kind,
            status,
            context: "",
            time: "",
            branch: "",
            provider: None,
            selected: false,
            unread: false,
            model: "",
            changes: None,
        }
    }

    // the faint line above the title, which turns amber or red when the session needs attention
    pub fn context(mut self, context: &'a str) -> Self {
        self.context = context;
        self
    }

    pub fn time(mut self, time: &'a str) -> Self {
        self.time = time;
        self
    }

    pub fn branch(mut self, branch: &'a str) -> Self {
        self.branch = branch;
        self
    }

    // shows the agent's own logo in place of the session type icon
    pub fn provider(mut self, provider: Provider) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    // the model an agent runs, named beside its mark
    pub fn model(mut self, model: &'a str) -> Self {
        self.model = model;
        self
    }

    pub fn changes(mut self, added: u32, removed: u32) -> Self {
        self.changes = Some((added, removed));
        self
    }

    // something happened here since it was last looked at, like an agent finishing
    pub fn unread(mut self, unread: bool) -> Self {
        self.unread = unread;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let size = Vec2::new(ui.available_width(), Self::HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, self.title));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let selected = spring_bool(
                ui.ctx(),
                response.id.with("selected"),
                self.selected,
                Spring::SMOOTH,
            );
            let fill = Color32::TRANSPARENT
                .lerp_to_gamma(theme::tint(p, 0.05), t)
                .lerp_to_gamma(theme::tint(p, 0.10), selected);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::LG), fill);

            let inner = rect.shrink2(Vec2::new(space::MD, 11.0));
            let small = font::regular(text::SM + 1.0);
            // each line sits on its own spot, so the title gets room above and below it
            let lines = [inner.top() + 7.0, inner.top() + 25.0, inner.bottom() - 7.0];
            let row = |i: f32| lines[i as usize];

            let time =
                ui.painter()
                    .layout_no_wrap(self.time.to_owned(), small.clone(), p.text_faint);
            let mut time_w = time.size().x;
            ui.painter().galley(
                pos2(inner.right() - time_w, row(0.0) - time.size().y / 2.0),
                time,
                p.text_faint,
            );
            let dot = spring_bool(
                ui.ctx(),
                response.id.with("unread"),
                self.unread,
                Spring::QUICK,
            );
            if dot > 0.01 {
                let center = pos2(inner.right() - time_w - space::SM - 3.0, row(0.0));
                ui.painter()
                    .circle_filled(center, 3.5 * dot, p.accent.gamma_multiply(dot));
                time_w += space::SM + 7.0;
            }
            let context = elided(
                ui,
                self.context,
                small.clone(),
                context_color(p, self.status),
                inner.width() - time_w - space::SM,
            );
            ui.painter().galley(
                pos2(inner.left(), row(0.0) - context.size().y / 2.0),
                context,
                p.text_faint,
            );

            let title = elided(
                ui,
                self.title,
                font::medium(text::MD + 1.0),
                p.text,
                inner.width(),
            );
            ui.painter().galley(
                pos2(inner.left(), row(1.0) - title.size().y / 2.0),
                title,
                p.text,
            );

            let mark_center = pos2(inner.left() + icon::SM / 2.0, row(2.0));
            if let Some(provider) = self.provider {
                let color = match self.status {
                    SessionStatus::Done | SessionStatus::Idle => {
                        provider_color(provider, p.text_faint)
                    }
                    status => status_color(p, status),
                };
                provider_mark(ui.painter(), mark_center, icon::SM, provider, color);
            } else {
                let glyph = match self.kind {
                    SessionKind::Agent => icons::ROBOT,
                    SessionKind::Shell => icons::TERMINAL,
                    SessionKind::Task => icons::PLAY,
                };
                ui.painter().text(
                    mark_center,
                    Align2::CENTER_CENTER,
                    glyph,
                    font::icon(icon::SM),
                    status_color(p, self.status),
                );
            }
            // the bottom line reads mark, model, branch, with what changed pinned to the right
            let y = row(2.0);
            let mut right = inner.right();
            if let Some((added, removed)) = self.changes.filter(|(a, r)| a + r > 0) {
                let minus = ui.painter().text(
                    pos2(right, y),
                    Align2::RIGHT_CENTER,
                    format!("−{removed}"),
                    small.clone(),
                    p.danger.gamma_multiply(0.85),
                );
                let plus = ui.painter().text(
                    pos2(minus.left() - space::XS, y),
                    Align2::RIGHT_CENTER,
                    format!("+{added}"),
                    small.clone(),
                    p.success.gamma_multiply(0.85),
                );
                right = plus.left() - space::SM;
            }
            let mut x = inner.left() + icon::SM + space::SM;
            if !self.model.is_empty() {
                let model = elided(ui, self.model, small.clone(), p.text_faint, right - x);
                let width = model.size().x;
                ui.painter()
                    .galley(pos2(x, y - model.size().y / 2.0), model, p.text_faint);
                x += width + space::SM;
            }
            if !self.branch.is_empty() && right - x > icon::SM + space::LG {
                ui.painter().text(
                    pos2(x + icon::SM / 2.0, y),
                    Align2::CENTER_CENTER,
                    icons::GIT_BRANCH,
                    font::icon(icon::SM),
                    p.text_faint,
                );
                let branch_x = x + icon::SM + space::XS;
                let branch = elided(ui, self.branch, small, p.text_faint, right - branch_x);
                ui.painter().galley(
                    pos2(branch_x, y - branch.size().y / 2.0),
                    branch,
                    p.text_faint,
                );
            }
        }

        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}

// the type icon only takes a colour when the session wants attention, otherwise it stays quiet
fn status_color(p: &Palette, status: SessionStatus) -> Color32 {
    match status {
        SessionStatus::Running => p.accent,
        SessionStatus::Waiting => p.warning,
        SessionStatus::Failed => p.danger,
        SessionStatus::Done | SessionStatus::Idle => p.text_faint,
    }
}

fn context_color(p: &Palette, status: SessionStatus) -> Color32 {
    match status {
        SessionStatus::Waiting => p.warning,
        SessionStatus::Failed => p.danger,
        _ => p.text_faint,
    }
}

// one line that ends in an ellipsis instead of being cut through a letter
fn elided(ui: &Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping {
        max_width: width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}
