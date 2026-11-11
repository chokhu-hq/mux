use super::*;

pub(super) const DIFF_PREVIEW: usize = 12;

// a file the agent changed, with the start of its changes, whose header opens the full diff
pub fn diff(ui: &mut Ui, diff: &FileDiff) -> bool {
    let mut open = false;
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("diff");
    let mut expanded = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let added = diff
        .lines
        .iter()
        .filter(|l| l.kind == DiffKind::Add)
        .count();
    let removed = diff
        .lines
        .iter()
        .filter(|l| l.kind == DiffKind::Remove)
        .count();

    card(ui, |ui| {
        let (rect, header) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
        let t = hover_t(ui, &header);
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius {
                nw: radius::LG,
                ne: radius::LG,
                sw: 0,
                se: 0,
            },
            p.hover.gamma_multiply(t * 0.6),
        );
        glyph(
            ui,
            pos2(rect.right() - space::MD - icon::MD / 2.0, rect.center().y),
            icons::ARROW_SQUARE_OUT,
            icon::MD,
            p.text_faint.lerp_to_gamma(p.text, t),
        );
        open = header
            .on_hover_cursor(CursorIcon::PointingHand)
            .tip("Open the full diff")
            .clicked();
        let y = rect.center().y;
        let x = rect.left() + space::MD;
        glyph(
            ui,
            pos2(x + icon::MD / 2.0, y),
            icons::GIT_DIFF,
            icon::MD,
            p.text_faint,
        );
        // the name and counts lead, and the folder fills what's left before the open icon
        let file = std::path::Path::new(&diff.path);
        let name = file
            .file_name()
            .map_or_else(|| diff.path.clone(), |n| n.to_string_lossy().into_owned());
        let name = mux_core::clip_lines(&name);
        let folder = file
            .parent()
            .map(|d| mux_core::display_path(&d.to_string_lossy()))
            .unwrap_or_default();
        let room = Rect::from_min_max(
            pos2(rect.left(), rect.top()),
            pos2(
                rect.right() - space::MD - icon::MD - space::SM,
                rect.bottom(),
            ),
        );
        let painter = ui.painter().with_clip_rect(room.intersect(ui.clip_rect()));
        let name = painter.text(
            pos2(x + icon::MD + space::SM, y),
            Align2::LEFT_CENTER,
            name,
            font::mono(text::MONO_SM),
            p.text,
        );
        let plus = painter.text(
            pos2(name.right() + space::SM, y),
            Align2::LEFT_CENTER,
            format!("+{added}"),
            font::medium(text::SM),
            p.success,
        );
        let minus = painter.text(
            pos2(plus.right() + space::XS, y),
            Align2::LEFT_CENTER,
            format!("−{removed}"),
            font::medium(text::SM),
            p.danger,
        );
        let left = minus.right() + space::SM;
        if room.right() - left > 24.0 {
            let mut job =
                LayoutJob::simple_singleline(folder, font::regular(text::SM + 1.0), p.text_faint);
            job.wrap = egui::text::TextWrapping {
                max_width: room.right() - left,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.painter().layout_job(job);
            ui.painter()
                .galley(pos2(left, y - galley.size().y / 2.0), galley, p.text_faint);
        }

        let line = ui.available_rect_before_wrap();
        ui.painter()
            .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
        let shown = if expanded {
            diff.lines.len()
        } else {
            diff.lines.len().min(DIFF_PREVIEW)
        };
        let row_h = 18.0;
        let mono = font::mono(text::MONO_SM);
        ui.add_space(space::XS);
        for line in &diff.lines[..shown] {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            let (bg, sign, color) = match line.kind {
                DiffKind::Add => (p.success.gamma_multiply(0.12), "+", p.success),
                DiffKind::Remove => (p.danger.gamma_multiply(0.12), "−", p.danger),
                DiffKind::Context => (Color32::TRANSPARENT, " ", p.text_faint),
            };
            ui.painter().rect_filled(rect, 0.0, bg);
            ui.painter().text(
                pos2(rect.left() + space::MD, rect.center().y),
                Align2::LEFT_CENTER,
                sign,
                mono.clone(),
                color,
            );
            let text_color = if line.kind == DiffKind::Context {
                p.text_muted
            } else {
                p.text
            };
            ui.painter()
                .with_clip_rect(rect.intersect(ui.clip_rect()))
                .text(
                    pos2(rect.left() + space::MD + 16.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    mux_core::clip_lines(&line.text),
                    mono.clone(),
                    text_color,
                );
        }
        ui.add_space(space::XS);

        let hidden = diff.lines.len() - shown;
        if hidden > 0 || expanded && diff.lines.len() > DIFF_PREVIEW {
            let label = if expanded {
                "Show less".to_owned()
            } else {
                format!("Show {hidden} more lines")
            };
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let t = hover_t(ui, &response);
            ui.painter()
                .hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.border));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                font::medium(text::SM),
                p.text_faint.lerp_to_gamma(p.text, t),
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                expanded = !expanded;
                ui.data_mut(|d| d.insert_temp(id, expanded));
            }
        }
    });
    open
}

// the end of a turn that changed several files, a click away from reviewing them
pub fn changes(ui: &mut Ui, changes: &Changes) -> bool {
    let p = theme::palette(ui);
    let mut open = false;
    card(ui, |ui| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
        let t = hover_t(ui, &response);
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(radius::LG),
            p.hover.gamma_multiply(t * 0.6),
        );
        let y = rect.center().y;
        let x = rect.left() + space::MD;
        glyph(
            ui,
            pos2(x + icon::MD / 2.0, y),
            icons::GIT_DIFF,
            icon::MD,
            p.text_muted,
        );
        let label = ui.painter().text(
            pos2(x + icon::MD + space::SM, y),
            Align2::LEFT_CENTER,
            format!("Changed {} files", changes.files),
            font::medium(text::MD),
            p.text,
        );
        let plus = ui.painter().text(
            pos2(label.right() + space::SM, y),
            Align2::LEFT_CENTER,
            format!("+{}", changes.added),
            font::medium(text::SM),
            p.success,
        );
        ui.painter().text(
            pos2(plus.right() + space::XS, y),
            Align2::LEFT_CENTER,
            format!("−{}", changes.removed),
            font::medium(text::SM),
            p.danger,
        );
        let review = ui.painter().text(
            pos2(rect.right() - space::MD - icon::MD - space::XS, y),
            Align2::RIGHT_CENTER,
            "Review",
            font::medium(text::SM + 1.0),
            p.text_muted.lerp_to_gamma(p.text, t),
        );
        glyph(
            ui,
            pos2(review.right() + space::XS + icon::MD / 2.0, y),
            icons::CARET_RIGHT,
            icon::MD,
            p.text_muted.lerp_to_gamma(p.text, t),
        );
        open = response
            .on_hover_cursor(CursorIcon::PointingHand)
            .tip("Step through every file this turn changed")
            .clicked();
    });
    open
}
