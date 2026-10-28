use std::time::{SystemTime, UNIX_EPOCH};

use egui::{
    Align2, CornerRadius, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke, Ui, pos2, vec2,
};
use mux_core::protocol::{Background, BackgroundState};

use super::cards::{ROW, caret, glyph, spinner};
use crate::components::{IconButton, TooltipExt, hover_t, reveal};
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

// commands the agent left running, above the composer so they stay in sight while the chat scrolls,
// returning the one to stop when its stop button is pressed
pub fn strip(ui: &mut Ui, tasks: &[Background]) -> Option<String> {
    if tasks.is_empty() {
        return None;
    }
    let p = theme::palette(ui);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut stop = None;
    Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(radius::LG)
        .inner_margin(Margin::same(space::XS as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for task in tasks {
                ui.push_id(&task.id, |ui| {
                    if row(ui, task, now) {
                        stop = Some(task.id.clone());
                    }
                });
            }
        });
    ui.add_space(space::SM);
    // the running time ticks over each second
    if tasks.iter().any(|t| t.state == BackgroundState::Running) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));
    }
    stop
}

// one command, with how long it has run or how it ended, its output a click away, and a stop button
fn row(ui: &mut Ui, task: &Background, now: u64) -> bool {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("open");
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let t = hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::MD),
        p.hover.gamma_multiply(t),
    );
    let y = rect.center().y;
    let center = pos2(rect.left() + space::SM + icon::MD / 2.0, y);
    let (status, status_color) = match task.state {
        BackgroundState::Running => {
            spinner(ui, center, icon::MD, p.text_muted);
            (elapsed(now.saturating_sub(task.started)), p.text_faint)
        }
        BackgroundState::Exited(code @ (Some(0) | None)) => {
            glyph(ui, center, icons::CHECK_CIRCLE, icon::MD, p.success);
            let said = code.map_or("Finished".to_owned(), |c| format!("Exited {c}"));
            (said, p.text_faint)
        }
        BackgroundState::Exited(Some(code)) => {
            glyph(ui, center, icons::X_CIRCLE, icon::MD, p.danger);
            (format!("Exited {code}"), p.danger)
        }
        BackgroundState::Stopped => {
            glyph(ui, center, icons::X_CIRCLE, icon::MD, p.text_faint);
            ("Stopped".to_owned(), p.text_faint)
        }
    };

    // from the right: the caret, the stop button, then the status, with the label taking what's left
    let turn = spring_bool(ui.ctx(), id.with("caret"), open, Spring::SMOOTH);
    caret(
        ui,
        pos2(rect.right() - space::MD - 6.0, y),
        turn,
        p.text_faint.gamma_multiply(t.max(turn).max(0.5)),
    );
    let mut right = rect.right() - space::MD - 12.0 - space::SM;
    let mut stopped = false;
    if task.stoppable {
        let size = control::SM;
        let button = Rect::from_center_size(pos2(right - size / 2.0, y), vec2(size, size));
        stopped = ui
            .scope_builder(egui::UiBuilder::new().max_rect(button), |ui| {
                IconButton::new(icons::STOP, "Stop").filled().show(ui)
            })
            .inner
            .tip("Stop this command")
            .clicked();
        right = button.left() - space::SM;
    }
    let status = ui.painter().text(
        pos2(right, y),
        Align2::RIGHT_CENTER,
        status,
        font::regular(text::SM + 1.0),
        status_color,
    );
    let left = center.x + icon::MD / 2.0 + space::SM;
    let mut job = egui::text::LayoutJob::simple_singleline(
        task.label.clone(),
        font::medium(text::MD),
        p.text_muted,
    );
    job.wrap =
        egui::text::TextWrapping::truncate_at_width((status.left() - space::MD - left).max(0.0));
    let label = ui.painter().layout_job(job);
    ui.painter()
        .galley(pos2(left, y - label.size().y / 2.0), label, p.text_muted);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if task.command != task.label && !task.command.is_empty() {
        response.tip(&task.command);
    }

    reveal(ui, id.with("output"), open, |ui| {
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(radius::MD)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let output = task.output.trim_end();
                if output.is_empty() {
                    ui.label(
                        RichText::new("No output yet")
                            .font(font::regular(text::SM + 1.0))
                            .color(p.text_faint),
                    );
                    return;
                }
                // the newest lines matter most, so it stays scrolled to the bottom as more arrive
                ScrollArea::vertical()
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(output)
                                .font(font::mono(text::MONO_SM))
                                .color(p.text_muted),
                        );
                    });
            });
        ui.add_space(space::XS);
    });
    stopped
}

fn elapsed(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs / 60 % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::elapsed;

    #[test]
    fn running_time_reads_naturally() {
        assert_eq!(elapsed(12), "12s");
        assert_eq!(elapsed(125), "2m 05s");
        assert_eq!(elapsed(4980), "1h 23m");
    }
}
