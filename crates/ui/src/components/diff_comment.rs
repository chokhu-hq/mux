use egui::{Align2, CornerRadius, CursorIcon, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

use super::{TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, control, font, icon, radius, space, text};

// a note left on some lines of a diff, waiting to go to the agent with the next message
#[derive(Clone, Debug, PartialEq)]
pub struct DiffComment {
    // the file as this app opens it, which the diff view matches its marks against
    pub path: String,
    // the file as the message names it, relative to the project when it can be
    pub file: String,
    // the lines as people read them, like +12 or +12 to +15
    pub range: String,
    // the old and new line numbers the comment covers, counted from 1 and inclusive
    pub old: Option<(usize, usize)>,
    pub new: Option<(usize, usize)>,
    pub text: String,
    // the commented lines as a hunk, with its own @@ header
    pub diff: String,
}

impl DiffComment {
    pub fn covers(&self, old: Option<usize>, new: Option<usize>) -> bool {
        let within = |n: Option<usize>, range: Option<(usize, usize)>| {
            n.zip(range)
                .is_some_and(|(n, (start, end))| (start..=end).contains(&n))
        };
        within(old, self.old) || within(new, self.new)
    }

    fn label(&self) -> String {
        format!("{} {}", mux_core::paths::file_name(&self.file), self.range)
    }
}

// the message with each comment after it, laid out the way t3 code hands review comments to an agent
pub fn with_comments(text: &str, comments: &[DiffComment]) -> String {
    if comments.is_empty() {
        return text.to_owned();
    }
    let mut out = String::new();
    if !text.trim().is_empty() {
        out.push_str(text.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("Review comments on the diff:");
    for comment in comments {
        out.push_str("\n\n");
        out.push_str(&format!(
            "file: {}\nrange: {}\n",
            comment.file, comment.range
        ));
        out.push_str("comment:\n");
        for line in comment.text.trim().lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        let diff = comment.diff.trim_end_matches('\n');
        // a fence longer than any run of backticks inside, so the diff can't close it early
        let longest = diff.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(longest.max(2) + 1);
        out.push_str(&format!("{fence}diff\n{diff}\n{fence}"));
    }
    out
}

// the comments waiting to be sent, as chips above the composer, each removable with its x
pub fn strip(ui: &mut Ui, comments: &mut Vec<DiffComment>) {
    if comments.is_empty() {
        return;
    }
    let mut removed = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::XS);
        for (i, comment) in comments.iter().enumerate() {
            if chip(ui, comment) {
                removed = Some(i);
            }
        }
    });
    if let Some(i) = removed {
        comments.remove(i);
    }
    ui.add_space(space::SM);
}

// returns true when its x is clicked
fn chip(ui: &mut Ui, comment: &DiffComment) -> bool {
    let p = theme::palette(ui);
    let label = comment.label();
    let galley = super::elided(
        ui,
        label.clone(),
        font::medium(text::SM + 1.0),
        p.text_muted,
        260.0,
    );
    let close = control::SM - 6.0;
    let width = space::SM + icon::SM + space::XS + galley.size().x + space::XS + close + 3.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, control::SM), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Comment on {label}"))
    });
    let x_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 3.0 - close / 2.0, rect.center().y),
        Vec2::splat(close),
    );
    let x = ui
        .interact(x_rect, response.id.with("remove"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    x.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            true,
            format!("Remove comment on {label}"),
        )
    });
    let rounding = CornerRadius::same(radius::MD);
    ui.painter()
        .rect_filled(rect, rounding, theme::tint(p, theme::fill::QUIET));
    ui.painter().rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    let y = rect.center().y;
    ui.painter().text(
        egui::pos2(rect.left() + space::SM + icon::SM / 2.0, y),
        Align2::CENTER_CENTER,
        icons::CHAT_CIRCLE,
        font::icon(icon::SM),
        p.accent,
    );
    let text_x = rect.left() + space::SM + icon::SM + space::XS;
    ui.painter().galley(
        egui::pos2(text_x, y - galley.size().y / 2.0),
        galley,
        p.text_muted,
    );
    let t = hover_t(ui, &x);
    ui.painter().rect_filled(
        x_rect,
        CornerRadius::same(radius::SM),
        p.hover.gamma_multiply(t),
    );
    ui.painter().text(
        x_rect.center(),
        Align2::CENTER_CENTER,
        icons::X,
        font::icon(icon::SM - 2.0),
        p.text_faint.lerp_to_gamma(p.text, t),
    );
    let removed = x.clicked();
    response.tip(format!(
        "{} {}\n\n{}",
        comment.file,
        comment.range,
        comment.text.trim()
    ));
    removed
}

#[cfg(test)]
mod tests {
    use super::{DiffComment, with_comments};

    fn comment(text: &str, diff: &str) -> DiffComment {
        DiffComment {
            path: "/p/src/auth.rs".to_owned(),
            file: "src/auth.rs".to_owned(),
            range: "+11 to +12".to_owned(),
            old: None,
            new: Some((11, 12)),
            text: text.to_owned(),
            diff: diff.to_owned(),
        }
    }

    #[test]
    fn puts_comments_after_the_message() {
        let comments = [comment(
            "Rename this\nto token",
            "@@ -11,0 +11,2 @@\n+let b = 3;\n+let c = 4;\n",
        )];
        assert_eq!(
            with_comments("Please fix these  \n", &comments),
            "Please fix these\n\nReview comments on the diff:\n\nfile: src/auth.rs\nrange: +11 to +12\ncomment:\n  Rename this\n  to token\n```diff\n@@ -11,0 +11,2 @@\n+let b = 3;\n+let c = 4;\n```"
        );
    }

    #[test]
    fn sends_comments_on_their_own_and_fences_around_backticks() {
        let comments = [comment("Why?", "@@ -1,1 +1,1 @@\n+let s = \"```\";")];
        let out = with_comments("", &comments);
        assert!(out.starts_with("Review comments on the diff:\n\nfile: src/auth.rs"));
        assert!(out.ends_with("````diff\n@@ -1,1 +1,1 @@\n+let s = \"```\";\n````"));
        assert_eq!(with_comments("hi", &[]), "hi");
    }

    #[test]
    fn covers_its_old_and_new_lines() {
        let mut c = comment("x", "");
        c.old = Some((4, 4));
        assert!(c.covers(None, Some(12)));
        assert!(c.covers(Some(4), None));
        assert!(!c.covers(Some(5), Some(13)));
    }
}
