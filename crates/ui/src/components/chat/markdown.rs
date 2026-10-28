use egui::text::LayoutJob;
use egui::{Frame, Label, Margin, RichText, ScrollArea, Stroke, TextFormat, Ui, vec2};

use crate::components::{IconButton, TooltipExt};
use crate::icons;
use crate::theme::{self, Palette, font, radius, space, text};

const LINE_HEIGHT: f32 = 20.0;

// a deliberately small subset: paragraphs, headings, lists, bold, inline code and fenced code blocks
pub fn markdown(ui: &mut Ui, source: &str) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        let mut lines = source.lines();
        let mut paragraph = String::new();
        let mut blocks = 0;
        while let Some(line) = lines.next() {
            let trimmed = line.trim_start();
            if let Some(lang) = trimmed.strip_prefix("```") {
                flush(ui, &mut paragraph);
                let mut code = String::new();
                for line in lines.by_ref() {
                    if line.trim_start().starts_with("```") {
                        break;
                    }
                    code.push_str(line);
                    code.push('\n');
                }
                ui.push_id(blocks, |ui| code_block(ui, lang.trim(), code.trim_end()));
                blocks += 1;
            } else if let Some(item) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                flush(ui, &mut paragraph);
                list_item(ui, "•", item);
            } else if let Some((number, item)) = trimmed.split_once(". ")
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
            {
                flush(ui, &mut paragraph);
                list_item(ui, &format!("{number}."), item);
            } else if trimmed.starts_with('#') {
                flush(ui, &mut paragraph);
                let heading = trimmed.trim_start_matches('#').trim();
                ui.label(RichText::new(heading).font(font::semibold(text::LG)));
            } else if trimmed.is_empty() {
                flush(ui, &mut paragraph);
            } else {
                if !paragraph.is_empty() {
                    paragraph.push(' ');
                }
                paragraph.push_str(trimmed);
            }
        }
        flush(ui, &mut paragraph);
    });
}

fn flush(ui: &mut Ui, paragraph: &mut String) {
    if paragraph.is_empty() {
        return;
    }
    let job = inline(theme::palette(ui), paragraph);
    ui.add(Label::new(job).wrap());
    paragraph.clear();
}

fn list_item(ui: &mut Ui, marker: &str, item: &str) {
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.add_space(space::XS);
        ui.add_sized(
            vec2(14.0, LINE_HEIGHT),
            Label::new(RichText::new(marker).color(p.text_muted)),
        );
        ui.add(Label::new(inline(p, item)).wrap());
    });
}

fn inline(p: &Palette, source: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let (mut bold, mut code) = (false, false);
    let mut buf = String::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            append(&mut job, &mut buf, p, bold, code);
            code = !code;
        } else if c == '*' && !code && chars.peek() == Some(&'*') {
            chars.next();
            append(&mut job, &mut buf, p, bold, code);
            bold = !bold;
        } else {
            buf.push(c);
        }
    }
    append(&mut job, &mut buf, p, bold, code);
    job
}

fn append(job: &mut LayoutJob, buf: &mut String, p: &Palette, bold: bool, code: bool) {
    if buf.is_empty() {
        return;
    }
    let format = if code {
        TextFormat {
            font_id: font::mono(text::MONO_SM),
            color: p.text,
            background: p.hover,
            ..Default::default()
        }
    } else {
        TextFormat {
            font_id: if bold {
                font::semibold(text::MD)
            } else {
                font::regular(text::MD)
            },
            color: p.text,
            line_height: Some(LINE_HEIGHT),
            ..Default::default()
        }
    };
    job.append(buf, 0.0, format);
    buf.clear();
}

fn code_block(ui: &mut Ui, lang: &str, code: &str) {
    let p = theme::palette(ui);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(radius::MD)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            Frame::new()
                .inner_margin(Margin {
                    left: 10,
                    right: 4,
                    top: 2,
                    bottom: 2,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if lang.is_empty() { "code" } else { lang })
                                .font(font::regular(text::SM))
                                .color(p.text_faint),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if IconButton::new(icons::COPY, "Copy")
                                .show(ui)
                                .tip("Copy")
                                .clicked()
                            {
                                ui.ctx().copy_text(code.to_owned());
                            }
                        });
                    });
                });
            let line = ui.available_rect_before_wrap();
            ui.painter()
                .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
            Frame::new()
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ScrollArea::horizontal().show(ui, |ui| {
                        ui.add(
                            Label::new(
                                RichText::new(code)
                                    .font(font::mono(text::MONO))
                                    .color(p.text),
                            )
                            .extend(),
                        );
                    });
                });
        });
}
