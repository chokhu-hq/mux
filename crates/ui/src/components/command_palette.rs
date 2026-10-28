use egui::{
    Align2, Color32, Context, CornerRadius, Frame, Id, Key, KeyboardShortcut, Margin, Modifiers,
    Order, Rect, Sense, Stroke, TextEdit, Vec2, pos2, vec2,
};

use super::Kbd;
use crate::motion::{Spring, spring_bool};
use crate::shortcut;
use crate::theme::{self, font, icon, radius, space, text};

pub const SHORTCUT: KeyboardShortcut = shortcut::app(Key::K);
const WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 36.0;
const MAX_ROWS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: Option<char>,
    pub shortcut: Option<KeyboardShortcut>,
}

impl Command {
    pub const fn new(id: &'static str, title: &'static str) -> Self {
        Self {
            id,
            title,
            icon: None,
            shortcut: None,
        }
    }

    pub const fn icon(mut self, icon: char) -> Self {
        self.icon = Some(icon);
        self
    }

    pub const fn shortcut(mut self, shortcut: KeyboardShortcut) -> Self {
        self.shortcut = Some(shortcut);
        self
    }
}

// somewhere the palette can jump to, like a session, found by typing part of its name
pub struct Destination {
    pub id: u64,
    pub title: String,
    // a faint note on the right, like the project it belongs to
    pub detail: String,
    pub icon: char,
}

pub enum Choice {
    Command(&'static str),
    Destination(u64),
}

enum Row<'a> {
    Command(&'a Command),
    Destination(&'a Destination),
}

impl Row<'_> {
    fn choice(&self) -> Choice {
        match self {
            Row::Command(c) => Choice::Command(c.id),
            Row::Destination(d) => Choice::Destination(d.id),
        }
    }
}

#[derive(Default)]
pub struct CommandPalette {
    open: bool,
    query: String,
    selected: usize,
}

impl CommandPalette {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn toggle(&mut self) {
        if self.open { self.close() } else { self.open() }
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        commands: &[Command],
        destinations: &[Destination],
    ) -> Option<Choice> {
        if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT)) {
            self.toggle();
        }
        if let Some(id) = dispatch_shortcuts(ctx, commands) {
            self.open = false;
            return Some(Choice::Command(id));
        }

        let id = Id::unique("mux-command-palette");
        let t = spring_bool(ctx, id.with("open"), self.open, Spring::SNAPPY).clamp(0.0, 1.2);
        if !self.open && t <= 0.001 {
            return None;
        }

        // with something typed, places come first since that's usually what's being looked for
        let commands = commands
            .iter()
            .filter(|c| fuzzy_match(c.title, &self.query))
            .map(Row::Command);
        let places = destinations
            .iter()
            .filter(|d| fuzzy_match(&d.title, &self.query))
            .map(Row::Destination);
        let matches: Vec<Row> = if self.query.trim().is_empty() {
            commands.chain(places).take(MAX_ROWS).collect()
        } else {
            places.chain(commands).take(MAX_ROWS).collect()
        };
        self.selected = self.selected.min(matches.len().saturating_sub(1));

        let mut chosen = None;
        if self.open {
            ctx.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    self.open = false;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) && !matches.is_empty() {
                    self.selected = (self.selected + 1) % matches.len();
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) && !matches.is_empty() {
                    self.selected = (self.selected + matches.len() - 1) % matches.len();
                }
                if i.consume_key(Modifiers::NONE, Key::Enter) {
                    chosen = matches.get(self.selected).map(Row::choice);
                }
            });
        }

        let screen = ctx.content_rect();
        egui::Area::new(id)
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .constrain(false)
            .interactable(self.open)
            .show(ctx, |ui| {
                let p = theme::palette(ui);
                let fade = t.min(1.0);
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_black_alpha((90.0 * fade) as u8),
                );
                let backdrop = ui.interact(screen, id.with("backdrop"), Sense::click());

                // drops in from slightly above, the way spotlight settles into place
                let top = screen.top() + screen.height() * 0.16 - (1.0 - t) * 12.0;
                let panel = Rect::from_min_size(
                    pos2(screen.center().x - WIDTH / 2.0, top),
                    vec2(WIDTH, 0.0),
                );
                let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(panel));
                panel_ui.multiply_opacity(fade);

                let frame = theme::floating(ui)
                    .corner_radius(CornerRadius::same(radius::XL))
                    .inner_margin(Margin::same(space::XS as i8))
                    .show(&mut panel_ui, |ui| {
                        ui.set_width(WIDTH - space::XS * 2.0);
                        let input = ui.add(
                            TextEdit::singleline(&mut self.query)
                                .hint_text("Type a command or a session's name…")
                                .font(font::regular(text::LG))
                                .frame(Frame::new().inner_margin(Margin::symmetric(10, 10)))
                                .desired_width(f32::INFINITY),
                        );
                        super::edit_menu(&input, &mut self.query);
                        if self.open && !input.has_focus() {
                            input.request_focus();
                        }
                        if input.changed() {
                            self.selected = 0;
                        }

                        let sep = ui.available_rect_before_wrap();
                        ui.painter()
                            .hline(sep.x_range(), sep.top(), Stroke::new(1.0, p.border));
                        if matches.is_empty() {
                            // lines up with the typed text and takes a row's height, so the panel doesn't jump
                            let (row, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), ROW_HEIGHT + space::SM),
                                Sense::hover(),
                            );
                            ui.painter().text(
                                pos2(row.left() + 10.0, row.center().y),
                                Align2::LEFT_CENTER,
                                format!("No commands or sessions match “{}”", self.query.trim()),
                                font::regular(text::MD),
                                p.text_faint,
                            );
                            return;
                        }
                        ui.add_space(space::XS);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, row_item) in matches.iter().enumerate() {
                            let (rect, row) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), ROW_HEIGHT),
                                Sense::click(),
                            );
                            if row.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                                self.selected = i;
                            }
                            if row.clicked() {
                                chosen = Some(row_item.choice());
                            }
                            let sel = spring_bool(
                                ui.ctx(),
                                row.id.with("sel"),
                                self.selected == i,
                                Spring::QUICK,
                            );
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(radius::LG),
                                p.accent.gamma_multiply(0.16 * sel),
                            );
                            let (title, icon, shortcut, detail) = match row_item {
                                Row::Command(c) => (c.title, c.icon, c.shortcut.as_ref(), ""),
                                Row::Destination(d) => {
                                    (d.title.as_str(), Some(d.icon), None, d.detail.as_str())
                                }
                            };
                            let mut text_x = rect.left() + 10.0;
                            if let Some(icon) = icon {
                                ui.painter().text(
                                    pos2(text_x, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    icon,
                                    font::icon(icon::LG),
                                    p.text_muted,
                                );
                                text_x += 26.0;
                            }
                            ui.painter().text(
                                pos2(text_x, rect.center().y),
                                Align2::LEFT_CENTER,
                                title,
                                font::regular(text::MD),
                                p.text,
                            );
                            if !detail.is_empty() {
                                ui.painter().text(
                                    pos2(rect.right() - 10.0, rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    detail,
                                    font::regular(text::SM + 1.0),
                                    p.text_faint,
                                );
                            }
                            if let Some(shortcut) = shortcut {
                                let kbd = Rect::from_min_max(
                                    pos2(rect.right() - 120.0, rect.top()),
                                    pos2(rect.right() - 8.0, rect.bottom()),
                                );
                                let mut kbd_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(kbd)
                                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                                );
                                kbd_ui.spacing_mut().item_spacing.x = 3.0;
                                for key in shortcut::keys(shortcut).into_iter().rev() {
                                    Kbd::new(key).show(&mut kbd_ui);
                                }
                            }
                        }
                    });

                // the backdrop sits under the panel, so ignore clicks that land on the panel's padding
                let outside = backdrop
                    .interact_pointer_pos()
                    .is_some_and(|pos| !frame.response.rect.contains(pos));
                if self.open && backdrop.clicked() && outside {
                    self.open = false;
                }
            });

        if chosen.is_some() {
            self.open = false;
        }
        chosen
    }
}

fn dispatch_shortcuts(ctx: &Context, commands: &[Command]) -> Option<&'static str> {
    let mut bound: Vec<(&KeyboardShortcut, &'static str)> = commands
        .iter()
        .filter_map(|c| Some((c.shortcut.as_ref()?, c.id)))
        .collect();
    bound.sort_by_key(|(s, _)| std::cmp::Reverse(shortcut::specificity(s.modifiers)));
    ctx.input_mut(|i| {
        bound
            .into_iter()
            .find(|(s, _)| i.consume_shortcut(s))
            .map(|(_, id)| id)
    })
}

fn fuzzy_match(title: &str, query: &str) -> bool {
    let mut chars = title.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|q| chars.any(|c| c == q))
}
