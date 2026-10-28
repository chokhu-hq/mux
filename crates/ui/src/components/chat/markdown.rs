use egui::text::LayoutJob;
use egui::{Frame, Label, Margin, RichText, ScrollArea, Stroke, TextFormat, Ui, vec2};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::components::{IconButton, TooltipExt};
use crate::icons;
use crate::theme::{self, Palette, font, icon, radius, space, text};

// chat reads better a size up from the rest of the ui, with room between lines
const SIZE: f32 = text::MD + 1.0;
const LINE_HEIGHT: f32 = 22.0;

// renders an agent's markdown, returning the target of a file link that was clicked
pub fn markdown(ui: &mut Ui, source: &str) -> Option<String> {
    let blocks = parse(source);
    let mut clicked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        render(ui, &blocks, 0, &mut clicked);
    });
    clicked
}

#[derive(Clone, Copy, Default)]
struct Style {
    strong: bool,
    emph: bool,
    strike: bool,
    code: bool,
}

struct Span {
    text: String,
    style: Style,
    // which of the run's links this text belongs to
    link: Option<usize>,
}

#[derive(Default)]
struct Run {
    spans: Vec<Span>,
    links: Vec<String>,
}

impl Run {
    fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }
}

enum Block {
    Paragraph(Run),
    Heading(HeadingLevel, Run),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Code {
        lang: String,
        code: String,
    },
    Table {
        head: Vec<Run>,
        rows: Vec<Vec<Run>>,
    },
    Rule,
    Image(String),
}

struct Item {
    // a task list item's box, ticked or not
    task: Option<bool>,
    blocks: Vec<Block>,
}

// the blocks that hold other blocks, open while their contents are read
enum Open {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    List(Option<u64>, Vec<Item>),
    Item(Option<bool>, Vec<Block>),
    Table {
        head: Vec<Run>,
        rows: Vec<Vec<Run>>,
        row: Vec<Run>,
    },
}

enum Text {
    Paragraph,
    Heading(HeadingLevel),
    Cell,
}

#[derive(Default)]
struct Builder {
    open: Vec<Open>,
    text: Option<(Text, Run)>,
    code: Option<(String, String)>,
    style: Style,
    // how deep in each style the text is, since they can nest
    depth: [u8; 3],
    link: Option<usize>,
    // an image's address while its alt text is read
    image: Option<String>,
}

impl Builder {
    fn push(&mut self, block: Block) {
        if let Some(Open::Root(blocks) | Open::Quote(blocks) | Open::Item(_, blocks)) =
            self.open.last_mut()
        {
            blocks.push(block)
        }
    }

    // a tight list item's words arrive without a paragraph around them, so one is made for them
    fn run(&mut self) -> &mut Run {
        if self.text.is_none() {
            self.text = Some((Text::Paragraph, Run::default()));
        }
        &mut self.text.as_mut().expect("just set").1
    }

    fn close_text(&mut self) {
        let Some((kind, run)) = self.text.take() else {
            return;
        };
        match kind {
            Text::Paragraph if !run.is_empty() => self.push(Block::Paragraph(run)),
            Text::Heading(level) => self.push(Block::Heading(level, run)),
            Text::Cell => {
                if let Some(Open::Table { row, .. }) = self.open.last_mut() {
                    row.push(run);
                }
            }
            _ => {}
        }
        self.link = None;
    }

    fn add(&mut self, text: &str) {
        let (style, link) = (self.style, self.link);
        let run = self.run();
        match run.spans.last_mut() {
            Some(last)
                if last.link == link
                    && !last.style.code
                    && !style.code
                    && (last.style.strong, last.style.emph, last.style.strike)
                        == (style.strong, style.emph, style.strike) =>
            {
                last.text.push_str(text)
            }
            _ => run.spans.push(Span {
                text: text.to_owned(),
                style,
                link,
            }),
        }
    }

    fn set_style(&mut self, which: usize, on: bool) {
        self.depth[which] = if on {
            self.depth[which] + 1
        } else {
            self.depth[which].saturating_sub(1)
        };
        self.style.strong = self.depth[0] > 0;
        self.style.emph = self.depth[1] > 0;
        self.style.strike = self.depth[2] > 0;
    }
}

fn parse(source: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut b = Builder {
        open: vec![Open::Root(Vec::new())],
        ..Builder::default()
    };
    for event in Parser::new_ext(source, options) {
        if let Some((_, code)) = &mut b.code {
            match event {
                Event::Text(text) => code.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, code) = b.code.take().unwrap_or_default();
                    b.push(Block::Code {
                        lang,
                        code: code.trim_end().to_owned(),
                    });
                }
                _ => {}
            }
            continue;
        }
        // an image's alt text isn't shown, only the picture or a link to it
        if b.image.is_some() {
            if let Event::End(TagEnd::Image) = event {
                let dest = b.image.take().unwrap_or_default();
                let path = dest.strip_prefix("file://").unwrap_or(&dest).to_owned();
                // a picture on this machine is shown, anything else becomes a link to it
                if path.starts_with('/') && mux_core::protocol::is_image(&path) {
                    b.close_text();
                    // the agent names the picture where the server runs, which may not be where this app does
                    b.push(Block::Image(mux_core::paths::local(&path)));
                } else {
                    let link = b.run().links.len();
                    b.run().links.push(dest.clone());
                    b.link = Some(link);
                    b.add(&dest);
                    b.link = None;
                }
            }
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    b.close_text();
                    b.text = Some((Text::Paragraph, Run::default()));
                }
                Tag::Heading { level, .. } => {
                    b.close_text();
                    b.text = Some((Text::Heading(level), Run::default()));
                }
                Tag::BlockQuote(_) => {
                    b.close_text();
                    b.open.push(Open::Quote(Vec::new()));
                }
                Tag::CodeBlock(kind) => {
                    b.close_text();
                    let lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    b.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    b.close_text();
                    b.open.push(Open::List(start, Vec::new()));
                }
                Tag::Item => b.open.push(Open::Item(None, Vec::new())),
                Tag::Table(_) => {
                    b.close_text();
                    b.open.push(Open::Table {
                        head: Vec::new(),
                        rows: Vec::new(),
                        row: Vec::new(),
                    });
                }
                Tag::TableCell => b.text = Some((Text::Cell, Run::default())),
                Tag::Strong => b.set_style(0, true),
                Tag::Emphasis => b.set_style(1, true),
                Tag::Strikethrough => b.set_style(2, true),
                Tag::Link { dest_url, .. } => {
                    let run = b.run();
                    run.links.push(dest_url.to_string());
                    b.link = Some(run.links.len() - 1);
                }
                Tag::Image { dest_url, .. } => b.image = Some(dest_url.to_string()),
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell => b.close_text(),
                TagEnd::BlockQuote(_) => {
                    b.close_text();
                    if let Some(Open::Quote(blocks)) = b.open.pop() {
                        b.push(Block::Quote(blocks));
                    }
                }
                TagEnd::Item => {
                    b.close_text();
                    if let Some(Open::Item(task, blocks)) = b.open.pop()
                        && let Some(Open::List(_, items)) = b.open.last_mut()
                    {
                        items.push(Item { task, blocks });
                    }
                }
                TagEnd::List(_) => {
                    b.close_text();
                    if let Some(Open::List(start, items)) = b.open.pop() {
                        b.push(Block::List { start, items });
                    }
                }
                TagEnd::TableHead => {
                    if let Some(Open::Table { head, row, .. }) = b.open.last_mut() {
                        *head = std::mem::take(row);
                    }
                }
                TagEnd::TableRow => {
                    if let Some(Open::Table { rows, row, .. }) = b.open.last_mut() {
                        rows.push(std::mem::take(row));
                    }
                }
                TagEnd::Table => {
                    if let Some(Open::Table { head, rows, .. }) = b.open.pop() {
                        b.push(Block::Table { head, rows });
                    }
                }
                TagEnd::Strong => b.set_style(0, false),
                TagEnd::Emphasis => b.set_style(1, false),
                TagEnd::Strikethrough => b.set_style(2, false),
                TagEnd::Link => b.link = None,
                _ => {}
            },
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => b.add(&text),
            Event::Code(text) => {
                let style = b.style;
                b.style.code = true;
                b.add(&text);
                b.style = style;
            }
            Event::SoftBreak => b.add(" "),
            Event::HardBreak => b.add("\n"),
            Event::Rule => {
                b.close_text();
                b.push(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Open::Item(task, _)) = b.open.last_mut() {
                    *task = Some(done);
                }
            }
            _ => {}
        }
    }
    b.close_text();
    // anything left open by a reply cut off mid stream still shows
    while b.open.len() > 1 {
        match b.open.pop() {
            Some(Open::Quote(blocks) | Open::Item(_, blocks)) => {
                for block in blocks {
                    b.push(block);
                }
            }
            Some(Open::List(start, items)) => b.push(Block::List { start, items }),
            Some(Open::Table { head, rows, .. }) => b.push(Block::Table { head, rows }),
            _ => {}
        }
    }
    if let Some((lang, code)) = b.code.take() {
        b.push(Block::Code { lang, code });
    }
    match b.open.pop() {
        Some(Open::Root(blocks)) => blocks,
        _ => Vec::new(),
    }
}

fn render(ui: &mut Ui, blocks: &[Block], depth: usize, clicked: &mut Option<String>) {
    let p = theme::palette(ui);
    for (i, block) in blocks.iter().enumerate() {
        ui.push_id(i, |ui| match block {
            Block::Paragraph(run) => {
                let (job, links) = layout(p, run, SIZE, false);
                if let Some(target) = text_with_links(ui, job, &links, true) {
                    *clicked = Some(target);
                }
            }
            Block::Heading(level, run) => {
                let size = match level {
                    HeadingLevel::H1 => text::TITLE,
                    HeadingLevel::H2 => text::LG + 2.0,
                    _ => text::LG,
                };
                ui.add_space(space::XS);
                let (job, links) = layout(p, run, size, true);
                if let Some(target) = text_with_links(ui, job, &links, true) {
                    *clicked = Some(target);
                }
            }
            Block::List { start, items } => list(ui, *start, items, depth, clicked),
            Block::Quote(blocks) => {
                let shown = ui.horizontal_top(|ui| {
                    ui.add_space(space::MD + 3.0);
                    ui.vertical(|ui| render(ui, blocks, depth, clicked));
                });
                let rect = shown.response.rect;
                ui.painter().vline(
                    rect.left() + 1.5,
                    rect.y_range(),
                    Stroke::new(3.0, p.border_strong),
                );
            }
            Block::Code { lang, code } => code_block(ui, lang, code),
            Block::Table { head, rows } => table(ui, head, rows, clicked),
            Block::Rule => {
                let (rect, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), space::MD),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, p.border));
            }
            Block::Image(path) => image(ui, path),
        });
    }
}

fn list(
    ui: &mut Ui,
    start: Option<u64>,
    items: &[Item],
    depth: usize,
    clicked: &mut Option<String>,
) {
    let p = theme::palette(ui);
    for (i, item) in items.iter().enumerate() {
        ui.push_id(("item", i), |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                ui.add_space(space::XS);
                let (marker_rect, _) =
                    ui.allocate_exact_size(vec2(16.0, LINE_HEIGHT), egui::Sense::hover());
                let center = marker_rect.center();
                match (item.task, start) {
                    (Some(done), _) => {
                        let (glyph, color) = if done {
                            (icons::CHECK_CIRCLE, p.success)
                        } else {
                            (icons::CIRCLE, p.text_faint)
                        };
                        ui.painter().text(
                            center,
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            font::icon(icon::MD),
                            color,
                        );
                    }
                    (None, Some(first)) => {
                        ui.painter().text(
                            marker_rect.right_center(),
                            egui::Align2::RIGHT_CENTER,
                            format!("{}.", first + i as u64),
                            font::regular(SIZE),
                            p.text_muted,
                        );
                    }
                    // nested bullets change shape so each level reads as its own
                    (None, None) => {
                        let radius = 2.5;
                        if depth.is_multiple_of(2) {
                            ui.painter().circle_filled(center, radius, p.text_muted);
                        } else {
                            ui.painter().circle_stroke(
                                center,
                                radius,
                                Stroke::new(1.2, p.text_muted),
                            );
                        }
                    }
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::XS;
                    render(ui, &item.blocks, depth + 1, clicked);
                });
            });
        });
    }
}

fn table(ui: &mut Ui, head: &[Run], rows: &[Vec<Run>], clicked: &mut Option<String>) {
    let p = theme::palette(ui);
    let columns = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    theme::card(ui)
        .corner_radius(radius::MD)
        .inner_margin(Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("table")
                    .num_columns(columns)
                    .spacing(vec2(space::XL, space::SM))
                    .min_row_height(LINE_HEIGHT)
                    .show(ui, |ui| {
                        for (row_index, row) in std::iter::once(head)
                            .chain(rows.iter().map(Vec::as_slice))
                            .enumerate()
                        {
                            let header = row_index == 0;
                            for cell in row {
                                let (job, links) = layout(p, cell, SIZE, header);
                                if let Some(target) = text_with_links(ui, job, &links, false) {
                                    *clicked = Some(target);
                                }
                            }
                            ui.end_row();
                            // a line under the header, like a printed table
                            if header {
                                let line = ui.available_rect_before_wrap();
                                ui.painter().hline(
                                    ui.min_rect().x_range(),
                                    line.top() - space::SM / 2.0,
                                    Stroke::new(1.0, p.border),
                                );
                            }
                        }
                    });
            });
        });
}

struct Link {
    // characters of the laid out text the link covers
    chars: std::ops::Range<usize>,
    target: String,
}

fn layout(p: &Palette, run: &Run, size: f32, bold: bool) -> (LayoutJob, Vec<Link>) {
    let mut job = LayoutJob::default();
    let mut links: Vec<Link> = Vec::new();
    for span in &run.spans {
        let start = job.text.chars().count();
        let strong = bold || span.style.strong;
        let color = if span.link.is_some() {
            p.accent
        } else {
            p.text
        };
        let mut format = if span.style.code {
            TextFormat {
                font_id: font::mono(text::MONO_SM),
                color: p.text,
                background: p.hover,
                expand_bg: 3.0,
                // the smaller mono font would otherwise sit on the bottom of the taller prose line
                valign: egui::Align::Center,
                ..Default::default()
            }
        } else {
            TextFormat {
                font_id: if strong {
                    font::semibold(size)
                } else if span.link.is_some() {
                    font::medium(size)
                } else {
                    font::regular(size)
                },
                color,
                italics: span.style.emph,
                line_height: Some(LINE_HEIGHT.max(size + 6.0)),
                ..Default::default()
            }
        };
        if span.style.strike {
            format.strikethrough = Stroke::new(1.0, color);
        }
        if span.style.code {
            // no breaks inside code, and no padding characters around it, since a line can break at those
            // and leave an empty box behind, so the background's own margin gives it room instead
            let code = span.text.replace(' ', "\u{00A0}");
            job.append(&code, 0.0, format);
        } else {
            job.append(&span.text, 0.0, format);
        }
        if let Some(index) = span.link
            && let Some(target) = run.links.get(index)
        {
            let end = job.text.chars().count();
            match links.last_mut() {
                Some(last) if last.target == *target && last.chars.end == start => {
                    last.chars.end = end
                }
                _ => links.push(Link {
                    chars: start..end,
                    target: target.clone(),
                }),
            }
        }
    }
    (job, links)
}

// one label, where the character under the pointer decides which link is hovered or clicked
fn text_with_links(ui: &mut Ui, mut job: LayoutJob, links: &[Link], wrap: bool) -> Option<String> {
    job.wrap.max_width = if wrap {
        ui.available_width()
    } else {
        f32::INFINITY
    };
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

// a block this long is shown plain, because colouring it would stall the frame it first appears in
const MAX_COLORED: usize = 64 * 1024;

#[derive(Default)]
struct Colorizer;

impl egui::cache::ComputerMut<(&str, &str, &Palette, bool), Option<egui::text::LayoutJob>>
    for Colorizer
{
    fn compute(
        &mut self,
        (code, lang, p, dark): (&str, &str, &Palette, bool),
    ) -> Option<egui::text::LayoutJob> {
        crate::highlight::colorize(code, lang, p, dark, font::mono(text::MONO))
    }
}

// coloured blocks are kept while they're on screen, so each is coloured once rather than every frame
type ColoredCode = egui::cache::FrameCache<Option<egui::text::LayoutJob>, Colorizer>;

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
                    let dark = ui.visuals().dark_mode;
                    let colored = (code.len() <= MAX_COLORED).then(|| {
                        ui.memory_mut(|m| {
                            m.caches
                                .cache::<ColoredCode>()
                                .get((code, lang, p, dark))
                                .clone()
                        })
                    });
                    ScrollArea::horizontal().show(ui, |ui| match colored.flatten() {
                        Some(job) => {
                            ui.add(Label::new(job).extend());
                        }
                        None => {
                            ui.add(
                                Label::new(
                                    RichText::new(code)
                                        .font(font::mono(text::MONO))
                                        .color(p.text),
                                )
                                .extend(),
                            );
                        }
                    });
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(run: &Run) -> String {
        run.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn reads_tables_nested_lists_and_inline_styles() {
        let source = "Intro with `code` and **bold**.\n\n| Agent | Result |\n|---|--:|\n| 1 | 55 |\n| 2 | 6765 |\n\n- one\n  - nested\n- [x] done\n\n> quoted\n\n---\n\n```rust\nfn main() {}\n```";
        let blocks = parse(source);
        assert!(
            matches!(&blocks[0], Block::Paragraph(run) if text(run) == "Intro with code and bold.")
        );
        let Block::Table { head, rows } = &blocks[1] else {
            panic!("expected a table");
        };
        assert_eq!(
            head.iter().map(text).collect::<Vec<_>>(),
            ["Agent", "Result"]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[1][1]), "6765");
        let Block::List { items, .. } = &blocks[2] else {
            panic!("expected a list");
        };
        assert!(matches!(items[0].blocks.get(1), Some(Block::List { .. })));
        assert_eq!(items[1].task, Some(true));
        assert!(matches!(&blocks[3], Block::Quote(_)));
        assert!(matches!(&blocks[4], Block::Rule));
        assert!(matches!(&blocks[5], Block::Code { lang, .. } if lang == "rust"));
    }

    #[test]
    fn keeps_links_and_local_pictures() {
        let blocks = parse("See [the store](/src/store.rs:12) for details.\n\n![shot](/tmp/a.png)");
        let Block::Paragraph(run) = &blocks[0] else {
            panic!("expected a paragraph");
        };
        assert_eq!(run.links, ["/src/store.rs:12"]);
        assert!(matches!(&blocks[1], Block::Image(path) if path == "/tmp/a.png"));
    }
}
