use std::collections::HashMap;

use eframe::egui::{self, Color32, CornerRadius, Rect, Sense, Stroke, pos2, vec2};
use mux_core::Provider;
use mux_core::protocol::{TokenTotals, UsageDay};
use mux_ui::theme::{self, Palette, font, icon, space, text};
use mux_ui::{Segmented, provider_color, provider_mark, short_tokens};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const CHART_HEIGHT: f32 = 160.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Today,
    Week,
    Month,
}

impl Range {
    fn days(self) -> i64 {
        match self {
            Range::Today => 1,
            Range::Week => 7,
            Range::Month => 30,
        }
    }
}

// the token section of the usage tab: headline numbers, a daily chart and the models that used them
pub fn show(ui: &mut egui::Ui, history: Option<&(i64, Vec<UsageDay>)>) {
    let p = theme::palette(ui);
    let range_id = ui.scope_id().with("token-range");
    let mut range = ui
        .data(|d| d.get_temp::<Range>(range_id))
        .unwrap_or(Range::Week);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 28.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new("Tokens")
                    .font(font::medium(text::SM + 1.0))
                    .color(p.text_faint),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                Segmented::new(
                    &mut range,
                    &[
                        (Range::Today, "Today"),
                        (Range::Week, "7 days"),
                        (Range::Month, "30 days"),
                    ],
                )
                .show(ui);
            });
        },
    );
    ui.data_mut(|d| d.insert_temp(range_id, range));
    ui.add_space(space::SM);

    let Some((today, days)) = history else {
        card(ui, |ui| {
            ui.add_space(space::LG);
            ui.label(
                egui::RichText::new("Reading Codex and Claude history…")
                    .font(font::regular(text::MD))
                    .color(p.text_muted),
            );
            ui.add_space(space::LG);
        });
        ui.add_space(space::XL);
        return;
    };
    let first = today - range.days() + 1;
    let in_range: Vec<&UsageDay> = days.iter().filter(|d| d.day >= first).collect();
    let mut total = TokenTotals::default();
    for day in &in_range {
        total.add(&day.tokens);
    }

    tiles(ui, &total);
    ui.add_space(space::MD);
    // a single day has nothing to compare against, so the chart keeps showing the week around it
    let chart_days = range.days().max(7);
    card(ui, |ui| chart(ui, days, *today - chart_days + 1, *today));
    ui.add_space(space::MD);
    card(ui, |ui| models(ui, &in_range, total.total()));
    ui.add_space(space::XL);
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    theme::card(ui)
        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

// cache reads are cheap and the share of input served from cache is the number worth watching
fn hit_rate(t: &TokenTotals) -> Option<f32> {
    let input = t.input + t.cached + t.cache_write;
    (input > 0).then(|| t.cached as f32 / input as f32)
}

fn tiles(ui: &mut egui::Ui, t: &TokenTotals) {
    let p = theme::palette(ui);
    let hit = hit_rate(t).map_or(String::new(), |r| {
        format!("{}% of input", (r * 100.0).round() as u32)
    });
    let reasoning = if t.reasoning > 0 {
        format!("{} thinking", short_tokens(t.reasoning))
    } else {
        String::new()
    };
    let items = [
        ("Total", t.total(), String::new()),
        (
            "Input",
            t.input,
            format!("{} cache writes", short_tokens(t.cache_write)),
        ),
        ("Cache reads", t.cached, hit),
        ("Output", t.output, reasoning),
    ];
    let gap = space::SM;
    let width = (ui.available_width() - gap * 3.0) / 4.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (label, value, detail) in items {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 76.0), Sense::hover());
            let rounding = CornerRadius::same(theme::radius::LG);
            ui.painter()
                .rect_filled(rect, rounding, theme::tint(p, theme::fill::CARD));
            ui.painter().rect_stroke(
                rect,
                rounding,
                Stroke::new(1.0, p.border),
                egui::StrokeKind::Inside,
            );
            let left = rect.left() + space::MD;
            ui.painter().text(
                pos2(left, rect.top() + space::MD),
                egui::Align2::LEFT_TOP,
                label,
                font::regular(text::SM + 1.0),
                p.text_muted,
            );
            ui.painter().text(
                pos2(left, rect.top() + 30.0),
                egui::Align2::LEFT_TOP,
                short_tokens(value),
                font::semibold(text::TITLE),
                p.text,
            );
            if !detail.is_empty() {
                ui.painter().text(
                    pos2(left, rect.bottom() - space::SM),
                    egui::Align2::LEFT_BOTTOM,
                    detail,
                    font::regular(text::SM),
                    p.text_faint,
                );
            }
        }
    });
}

// opencode keeps its history in a database rather than transcripts, so only these two are counted
const CHARTED: [Provider; 2] = [Provider::Codex, Provider::Claude];

// each agent keeps one colour wherever it appears, checked for colour blind separation against both surfaces
fn series_color(p: &Palette, dark: bool, provider: Provider) -> Color32 {
    match (provider, dark) {
        (Provider::Codex, _) => p.accent,
        (Provider::Claude, true) => Color32::from_rgb(0xC9, 0x6A, 0x4C),
        (Provider::Claude, false) => mux_ui::CLAUDE_ORANGE,
        (Provider::OpenCode, _) => p.text_muted,
    }
}

fn chart(ui: &mut egui::Ui, days: &[UsageDay], first: i64, last: i64) {
    let p = theme::palette(ui);
    let dark = ui.visuals().dark_mode;
    let mut per_day: HashMap<(i64, Provider), TokenTotals> = HashMap::new();
    for d in days {
        per_day
            .entry((d.day, d.provider))
            .or_default()
            .add(&d.tokens);
    }
    let count = (last - first + 1) as usize;
    let split: Vec<[TokenTotals; 2]> = (first..=last)
        .map(|day| CHARTED.map(|pr| per_day.get(&(day, pr)).copied().unwrap_or_default()))
        .collect();
    let totals: Vec<[u64; 2]> = split.iter().map(|t| t.map(|t| t.total())).collect();
    let peak = totals.iter().map(|t| t[0] + t[1]).max().unwrap_or(0);
    let top = nice_ceiling(peak);

    // the title and legend share a row, since two series always need a key
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Daily tokens")
                .font(font::medium(text::MD))
                .color(p.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for provider in CHARTED.into_iter().rev() {
                ui.label(
                    egui::RichText::new(provider.label())
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_muted),
                );
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().rect_filled(
                    dot,
                    CornerRadius::same(theme::radius::SM),
                    series_color(p, dark, provider),
                );
                ui.add_space(space::SM);
            }
        });
    });
    ui.add_space(space::MD);

    let axis = 40.0;
    let (area, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), CHART_HEIGHT + 20.0),
        Sense::hover(),
    );
    let plot = Rect::from_min_max(
        pos2(area.left() + axis, area.top() + 6.0),
        pos2(area.right(), area.bottom() - 20.0),
    );
    let painter = ui.painter();
    // three quiet gridlines, labelled at the left, so the bars carry the eye
    for step in 0..=2 {
        let value = top / 2 * step;
        let y = plot.bottom() - plot.height() * step as f32 / 2.0;
        painter.hline(
            plot.x_range(),
            y,
            Stroke::new(1.0, if step == 0 { p.border_strong } else { p.border }),
        );
        painter.text(
            pos2(plot.left() - space::SM, y),
            egui::Align2::RIGHT_CENTER,
            if value == 0 {
                "0".to_owned()
            } else {
                short_tokens(value)
            },
            font::regular(text::SM),
            p.text_faint,
        );
    }

    let slot = plot.width() / count as f32;
    let bar_width = (slot * 0.62).clamp(4.0, 28.0);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut hovered = None;
    for (i, values) in totals.iter().enumerate() {
        let center = plot.left() + slot * (i as f32 + 0.5);
        let column =
            Rect::from_center_size(pos2(center, plot.center().y), vec2(slot, plot.height()));
        let hover = pointer.is_some_and(|pos| column.contains(pos));
        if hover {
            hovered = Some(i);
            painter.rect_filled(column, CornerRadius::same(theme::radius::SM), p.hover);
        }
        let mut base = plot.bottom();
        let segments: Vec<(Provider, u64)> = CHARTED
            .into_iter()
            .zip(values.iter().copied())
            .filter(|(_, v)| *v > 0)
            .collect();
        for (n, (provider, value)) in segments.iter().enumerate() {
            let height = plot.height() * (*value as f32 / top.max(1) as f32);
            let height = height.max(2.0);
            let last_segment = n + 1 == segments.len();
            // a 2px surface gap separates stacked segments, and only the top one is rounded
            let gap = if n > 0 { 2.0 } else { 0.0 };
            let rect = Rect::from_min_max(
                pos2(center - bar_width / 2.0, base - height),
                pos2(center + bar_width / 2.0, base - gap),
            );
            let rounding = if last_segment {
                CornerRadius {
                    nw: theme::radius::SM,
                    ne: theme::radius::SM,
                    sw: 0,
                    se: 0,
                }
            } else {
                CornerRadius::ZERO
            };
            painter.rect_filled(rect, rounding, series_color(p, dark, *provider));
            base -= height;
        }
        // a few dates along the bottom are enough to place the rest
        let labelled = i == 0 || i + 1 == count || (count > 7 && i == count / 2);
        if labelled {
            painter.text(
                pos2(center, plot.bottom() + 6.0),
                egui::Align2::CENTER_TOP,
                date(first + i as i64),
                font::regular(text::SM),
                p.text_faint,
            );
        }
    }

    let card_id = ui.scope_id().with("day-card");
    let shown = mux_ui::motion::spring_bool(
        ui.ctx(),
        card_id,
        hovered.is_some(),
        mux_ui::motion::Spring::QUICK,
    );
    // the last day hovered stays put while the card fades out
    let last_id = card_id.with("last");
    if let Some(i) = hovered {
        ui.data_mut(|d| d.insert_temp(last_id, i));
    }
    if shown > 0.01
        && let Some(i) = hovered.or_else(|| ui.data(|d| d.get_temp::<usize>(last_id)))
        && i < count
    {
        let column = Rect::from_center_size(
            pos2(plot.left() + slot * (i as f32 + 0.5), plot.center().y),
            vec2(slot, plot.height()),
        );
        day_card(
            ui,
            card_id,
            column,
            plot,
            first + i as i64,
            &split[i],
            shown,
        );
    }
}

const CARD_WIDTH: f32 = 236.0;

// a card beside the hovered day with its total, each agent's share and how the tokens split
fn day_card(
    ui: &egui::Ui,
    id: egui::Id,
    column: Rect,
    plot: Rect,
    day: i64,
    split: &[TokenTotals; 2],
    shown: f32,
) {
    let p = theme::palette(ui);
    let dark = ui.visuals().dark_mode;
    let mut total = TokenTotals::default();
    for t in split {
        total.add(t);
    }
    let breakdown = [
        ("Input", total.input),
        ("Cache reads", total.cached),
        ("Cache writes", total.cache_write),
        ("Output", total.output),
    ];
    let pad = space::MD;
    let row = 22.0;
    let height = pad
        + 18.0
        + 30.0
        + space::SM
        + row * 2.0
        + space::SM
        + 1.0
        + space::SM
        + row * breakdown.len() as f32
        + pad;
    // sits beside the day, on whichever side has room, and glides between days
    let right_side = column.center().x < plot.center().x;
    let x = if right_side {
        column.right() + space::SM
    } else {
        column.left() - space::SM - CARD_WIDTH
    };
    let x = ui.ctx().animate_value_with_time(id.with("x"), x, 0.08);
    let rect = Rect::from_min_size(pos2(x, plot.top() - space::SM), vec2(CARD_WIDTH, height))
        .translate(vec2(0.0, (1.0 - shown) * 4.0));
    let painter = ui
        .ctx()
        .layer_painter(egui::LayerId::new(egui::Order::Tooltip, id))
        .with_clip_rect(ui.ctx().content_rect());
    let fade = |c: Color32| c.gamma_multiply(shown);
    let rounding = CornerRadius::same(theme::radius::LG);
    let mut shadow = ui.visuals().popup_shadow;
    shadow.color = fade(shadow.color);
    painter.add(shadow.as_shape(rect, rounding));
    painter.rect_filled(rect, rounding, fade(p.elevated));
    painter.rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0, fade(p.border_strong)),
        egui::StrokeKind::Inside,
    );

    let left = rect.left() + pad;
    let right = rect.right() - pad;
    let mut y = rect.top() + pad;
    painter.text(
        pos2(left, y),
        egui::Align2::LEFT_TOP,
        long_date(day),
        font::regular(text::SM + 1.0),
        fade(p.text_muted),
    );
    y += 18.0;
    painter.text(
        pos2(left, y),
        egui::Align2::LEFT_TOP,
        short_tokens(total.total()),
        font::semibold(text::TITLE),
        fade(p.text),
    );
    painter.text(
        pos2(right, y + 13.0),
        egui::Align2::RIGHT_CENTER,
        hit_rate(&total).map_or(String::new(), |r| {
            format!("{}% cached", (r * 100.0).round() as u32)
        }),
        font::regular(text::SM + 1.0),
        fade(p.text_faint),
    );
    y += 30.0 + space::SM;

    for (provider, tokens) in CHARTED.into_iter().zip(split) {
        let mid = y + row / 2.0;
        painter.rect_filled(
            Rect::from_center_size(pos2(left + 4.0, mid), vec2(8.0, 8.0)),
            CornerRadius::same(theme::radius::SM),
            fade(series_color(p, dark, provider)),
        );
        painter.text(
            pos2(left + 16.0, mid),
            egui::Align2::LEFT_CENTER,
            provider.label(),
            font::regular(text::MD),
            fade(p.text),
        );
        let share = if total.total() == 0 {
            0.0
        } else {
            tokens.total() as f64 / total.total() as f64 * 100.0
        };
        // a sliver of the day still shows as there rather than rounding to nothing
        let share = if share > 0.0 && share < 0.5 {
            "<1%".to_owned()
        } else {
            format!("{}%", share.round() as u32)
        };
        painter.text(
            pos2(right, mid),
            egui::Align2::RIGHT_CENTER,
            short_tokens(tokens.total()),
            font::medium(text::MD),
            fade(p.text),
        );
        painter.text(
            pos2(right - 56.0, mid),
            egui::Align2::RIGHT_CENTER,
            share,
            font::regular(text::SM + 1.0),
            fade(p.text_faint),
        );
        y += row;
    }
    y += space::SM;
    painter.hline(left..=right, y, Stroke::new(1.0, fade(p.border)));
    y += 1.0 + space::SM;

    for (label, value) in breakdown {
        let mid = y + row / 2.0;
        painter.text(
            pos2(left, mid),
            egui::Align2::LEFT_CENTER,
            label,
            font::regular(text::SM + 1.0),
            fade(p.text_muted),
        );
        painter.text(
            pos2(right, mid),
            egui::Align2::RIGHT_CENTER,
            short_tokens(value),
            font::regular(text::SM + 1.0),
            fade(p.text),
        );
        y += row;
    }
}

fn models(ui: &mut egui::Ui, days: &[&UsageDay], total: u64) {
    let p = theme::palette(ui);
    ui.label(
        egui::RichText::new("By model")
            .font(font::medium(text::MD))
            .color(p.text),
    );
    ui.add_space(space::SM);
    let mut per_model: HashMap<(Provider, &str), TokenTotals> = HashMap::new();
    for d in days {
        per_model
            .entry((d.provider, d.model.as_str()))
            .or_default()
            .add(&d.tokens);
    }
    let mut rows: Vec<_> = per_model.into_iter().collect();
    rows.sort_by_key(|(_, t)| std::cmp::Reverse(t.total()));
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("No tokens used in this period")
                .font(font::regular(text::MD))
                .color(p.text_faint),
        );
        return;
    }
    for ((provider, model), tokens) in rows {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
        let painter = ui.painter();
        let y = rect.center().y;
        provider_mark(
            painter,
            pos2(rect.left() + icon::MD / 2.0, y),
            icon::MD,
            provider,
            provider_color(provider, p.text_muted),
        );
        let name_left = rect.left() + icon::MD + space::SM;
        painter.text(
            pos2(name_left, y),
            egui::Align2::LEFT_CENTER,
            model,
            font::regular(text::MD),
            p.text,
        );
        let share = if total == 0 {
            0.0
        } else {
            tokens.total() as f32 / total as f32
        };
        // the share bar is one quiet hue, since the mark beside the name already says which agent it is
        // columns from the right: the total, then the cache rate, then the bar filling what's left
        let (total_width, cached_width) = (72.0, 96.0);
        let bar = Rect::from_min_max(
            pos2(rect.left() + rect.width() * 0.36, y - 3.0),
            pos2(
                rect.right() - total_width - cached_width - space::MD,
                y + 3.0,
            ),
        );
        painter.rect_filled(
            bar,
            CornerRadius::same(theme::radius::FULL),
            theme::tint(p, theme::fill::TRACK),
        );
        painter.rect_filled(
            Rect::from_min_size(bar.min, vec2((bar.width() * share).max(2.0), bar.height())),
            CornerRadius::same(theme::radius::FULL),
            p.text_muted,
        );
        let hit = hit_rate(&tokens).map_or(String::new(), |r| {
            format!("{}% cached", (r * 100.0).round() as u32)
        });
        painter.text(
            pos2(rect.right(), y),
            egui::Align2::RIGHT_CENTER,
            short_tokens(tokens.total()),
            font::medium(text::MD),
            p.text,
        );
        painter.text(
            pos2(rect.right() - total_width, y),
            egui::Align2::RIGHT_CENTER,
            hit,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
    }
}

// rounds the tallest day up to a 1, 2 or 5 step so gridline labels read cleanly
fn nice_ceiling(value: u64) -> u64 {
    if value == 0 {
        return 2;
    }
    let mut step = 1u64;
    while step * 10 <= value {
        step *= 10;
    }
    for m in [1, 2, 5, 10] {
        // an even top keeps the middle gridline on a whole number
        if step * m >= value && (step * m).is_multiple_of(2) {
            return step * m;
        }
    }
    step * 10
}

// `Tuesday, Oct 21`, where the epoch fell on a thursday
fn long_date(day: i64) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ];
    format!("{}, {}", WEEKDAYS[day.rem_euclid(7) as usize], date(day))
}

// days since the epoch as `Oct 21`, after Howard Hinnant's civil_from_days
fn date(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{} {d}", MONTHS[(m - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::{date, long_date, nice_ceiling};

    #[test]
    fn formats_days_and_rounds_chart_tops() {
        assert_eq!(date(0), "Jan 1");
        assert_eq!(date(20_747), "Oct 21");
        assert_eq!(long_date(20_747), "Wednesday, Oct 21");
        assert_eq!(nice_ceiling(0), 2);
        assert_eq!(nice_ceiling(7), 10);
        assert_eq!(nice_ceiling(1_400_000), 2_000_000);
        assert_eq!(nice_ceiling(3_100_000), 5_000_000);
    }
}
