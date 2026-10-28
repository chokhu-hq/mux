use egui::text::LayoutJob;
use egui::{Align, Label, RichText, Ui};

use super::{Button, Icon};
use crate::theme::{self, font, icon, space, text};

const DETAIL_WIDTH: f32 = 420.0;

// the centred icon, title, note and button shown wherever a view has nothing to show yet
pub struct EmptyState<'a> {
    glyph: char,
    title: &'a str,
    detail: Option<String>,
    action: Option<&'a str>,
}

impl<'a> EmptyState<'a> {
    pub fn new(glyph: char, title: &'a str) -> Self {
        Self {
            glyph,
            title,
            detail: None,
            action: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    // returns true when the action button was clicked
    pub fn show(self, ui: &mut Ui) -> bool {
        let p = theme::palette(ui);
        let mut clicked = false;
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.35);
            Icon::new(self.glyph)
                .size(icon::XL)
                .color(p.text_faint)
                .show(ui);
            ui.add_space(space::SM);
            ui.label(
                RichText::new(self.title)
                    .font(font::medium(text::MD))
                    .color(p.text),
            );
            if let Some(detail) = self.detail {
                ui.add_space(space::XS);
                // laid out up front at a readable width, then centred as one block
                let mut job =
                    LayoutJob::simple(detail, font::regular(text::MD), p.text_muted, DETAIL_WIDTH);
                job.halign = Align::Center;
                let galley = ui.painter().layout_job(job);
                ui.add(Label::new(galley));
            }
            if let Some(action) = self.action {
                ui.add_space(space::MD);
                clicked = Button::new(action).primary().show(ui).clicked();
            }
        });
        clicked
    }
}
