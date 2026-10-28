use egui::text::LayoutJob;
use egui::{Frame, Label, Margin, RichText, ScrollArea, Stroke, TextFormat, Ui, vec2};

use crate::components::{IconButton, TooltipExt};
use crate::icons;
use crate::theme::{self, Palette, font, radius, space, text};

// chat reads better a size up from the rest of the ui, with room between lines
const SIZE: f32 = text::MD + 1.0;
const LINE_HEIGHT: f32 = 22.0;

// a deliberately small subset: paragraphs, headings, lists, bold, inline code, links and fenced code blocks,
// returning the target of a file link that was clicked
pub fn markdown(ui: &mut Ui, source: &str) -> Option<String> {
    let mut clicked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        let mut lines = source.lines();
        let mut paragraph = String::new();
        let mut blocks = 0;
        while let Some(line) = lines.next() {
            let trimmed = line.trim_start();
            if let Some(lang) = trimmed.strip_prefix("```") {
                flush(ui, &mut paragraph, &mut clicked);
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
                flush(ui, &mut paragraph, &mut clicked);
                list_item(ui, "•", item, &mut clicked);
            } else if let Some((number, item)) = trimmed.split_once(". ")
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
            {
                flush(ui, &mut paragraph, &mut clicked);
                list_item(ui, &format!("{number}."), item, &mut clicked);
            } else if let Some(path) = image_path(trimmed) {
                flush(ui, &mut paragraph, &mut clicked);
                ui.push_id(("image", blocks), |ui| image(ui, path));
                blocks += 1;
            } else if trimmed.starts_with('#') {
                flush(ui, &mut paragraph, &mut clicked);
                let heading = trimmed.trim_start_matches('#').trim();
                ui.label(RichText::new(heading).font(font::semibold(text::LG)));
            } else if trimmed.is_empty() {
                flush(ui, &mut paragraph, &mut clicked);
            } else {
                if !paragraph.is_empty() {
                    paragraph.push(' ');
                }
                paragraph.push_str(trimmed);
            }
        }
        flush(ui, &mut paragraph, &mut clicked);
    });
    clicked
}

// a line that is just ![alt](path), pointing at a picture on this machine
fn image_path(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("![")?;
    let (_, rest) = rest.split_once("](")?;
    let path = rest.strip_suffix(')')?.trim();
    let path = path.strip_prefix("file://").unwrap_or(path);
    (path.starts_with('/') && mux_core::protocol::is_image(path)).then_some(path)
}

fn image(ui: &mut Ui, path: &str) {
    let size = super::attachments::fitted(ui, path, vec2(ui.available_width().min(480.0), 360.0));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    super::attachments::picture(ui, path, rect, false);
    let response = response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .tip(mux_core::display_path(path));
    if response.clicked() {
        crate::components::ImageViewer::open(ui.ctx(), vec![path.to_owned()], 0);
    }
}

fn flush(ui: &mut Ui, paragraph: &mut String, clicked: &mut Option<String>) {
    if paragraph.is_empty() {
        return;
    }
    let (job, links) = inline(theme::palette(ui), paragraph);
    if let Some(target) = text_with_links(ui, job, &links) {
        *clicked = Some(target);
    }
    paragraph.clear();
}

struct Link {
    // characters of the laid out text the link covers
    chars: std::ops::Range<usize>,
    target: String,
}

// one wrapped label, where the character under the pointer decides which link is hovered or clicked
fn text_with_links(ui: &mut Ui, mut job: LayoutJob, links: &[Link]) -> Option<String> {
    job.wrap.max_width = ui.available_width();
    let galley = ui.painter().layout_job(job);
    let sense = if links.is_empty() {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let response = ui.add(Label::new(galley.clone()).sense(sense));
    if links.is_empty() {
        return None;
    }
    let pos = response.hover_pos()? - response.rect.min;
    // past the end of a line counts as no link, not as the line's last character
    let on_text = galley.rows.iter().any(|row| {
        row.rect()
            .expand2(vec2(0.0, 1.0))
            .contains(egui::pos2(pos.x, pos.y))
    });
    if !on_text {
        return None;
    }
    let index = galley.cursor_from_pos(pos).index.0;
    let link = links.iter().find(|l| l.chars.contains(&index))?;
    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    let web = link.target.starts_with("http://") || link.target.starts_with("https://");
    let shown = if web {
        link.target.clone()
    } else {
        mux_core::display_path(&link.target)
    };
    egui::Tooltip::always_open(
        ui.ctx().clone(),
        ui.layer_id(),
        response.id.with("link"),
        egui::PopupAnchor::Pointer,
    )
    .gap(12.0)
    .show(|ui| {
        ui.label(
            RichText::new(shown)
                .font(font::regular(text::SM))
                .color(theme::palette(ui).text),
        );
    });
    if !response.clicked() {
        return None;
    }
    if web {
        ui.ctx()
            .open_url(egui::OpenUrl::new_tab(link.target.clone()));
        return None;
    }
    Some(link.target.clone())
}

fn list_item(ui: &mut Ui, marker: &str, item: &str, clicked: &mut Option<String>) {
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.add_space(space::XS);
        ui.add_sized(
            vec2(14.0, LINE_HEIGHT),
            Label::new(RichText::new(marker).color(p.text_muted)),
        );
        let (job, links) = inline(p, item);
        if let Some(target) = text_with_links(ui, job, &links) {
            *clicked = Some(target);
        }
    });
}

fn inline(p: &Palette, source: &str) -> (LayoutJob, Vec<Link>) {
    let mut job = LayoutJob::default();
    let mut links = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut buf = String::new();
    let mut rest = source;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            append(&mut job, &mut buf, p, bold, code);
            code = !code;
            rest = &rest[1..];
        } else if !code && rest.starts_with("**") {
            append(&mut job, &mut buf, p, bold, code);
            bold = !bold;
            rest = &rest[2..];
        } else if !code
            && c == '['
            && let Some((label, target, len)) = link_at(rest)
        {
            append(&mut job, &mut buf, p, bold, code);
            let start = job.text.chars().count();
            job.append(
                &label.replace('`', ""),
                0.0,
                TextFormat {
                    font_id: if bold {
                        font::semibold(SIZE)
                    } else {
                        font::medium(SIZE)
                    },
                    color: p.accent,
                    line_height: Some(LINE_HEIGHT),
                    ..Default::default()
                },
            );
            links.push(Link {
                chars: start..job.text.chars().count(),
                target: target.to_owned(),
            });
            rest = &rest[len..];
        } else {
            buf.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    append(&mut job, &mut buf, p, bold, code);
    (job, links)
}

// [label](target) at the start of `text`, with the bytes it spans
fn link_at(text: &str) -> Option<(&str, &str, usize)> {
    let close = text.find("](")?;
    let label = &text[1..close];
    if label.is_empty() || label.contains('[') || label.contains(']') {
        return None;
    }
    let after = &text[close + 2..];
    let end = after.find(')')?;
    let target = after[..end].trim();
    if target.is_empty() || target.contains(' ') {
        return None;
    }
    Some((label, target, close + 2 + end + 1))
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
            expand_bg: 2.0,
            // the smaller mono font would otherwise sit on the bottom of the taller prose line
            valign: egui::Align::Center,
            ..Default::default()
        }
    } else {
        TextFormat {
            font_id: if bold {
                font::semibold(SIZE)
            } else {
                font::regular(SIZE)
            },
            color: p.text,
            line_height: Some(LINE_HEIGHT),
            ..Default::default()
        }
    };
    if code {
        // thin spaces inside the background give the code a little room on each side
        job.append(&format!("\u{2009}{buf}\u{2009}"), 0.0, format);
    } else {
        job.append(buf, 0.0, format);
    }
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
