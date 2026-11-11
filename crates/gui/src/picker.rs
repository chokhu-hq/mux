use eframe::egui::{self, Align2, CornerRadius, Rect, Sense, Stroke, pos2, vec2};
use mux_core::protocol::{AgentSettings, ModelOption};
use mux_core::{Access, Provider};
use mux_ui::components::{Chip, Segmented, TooltipExt, provider_color, provider_mark};
use mux_ui::icons;
use mux_ui::motion::{self, Spring};
use mux_ui::theme::{self, font, icon, radius, space, text};

use crate::sessions::Models;

const RAIL: f32 = 52.0;
const LIST: f32 = 300.0;
const SIDE: f32 = 210.0;
const BODY: f32 = 330.0;
const BAR: f32 = 52.0;
const HEADER: f32 = 48.0;
const ROW: f32 = 36.0;

// the model chip beside send, opening one panel for the agent, model, reasoning, speed and permissions
pub fn show(
    ui: &mut egui::Ui,
    models: &Models,
    settings: &AgentSettings,
    locked: bool,
) -> Option<AgentSettings> {
    let pick = |provider: Provider| pick(models, settings, provider);
    let current = pick(settings.provider);
    let name = current.map_or(settings.provider.label(), |m| m.name.as_str());
    let mut detail = current
        .filter(|m| !m.efforts.is_empty())
        .map(|m| effort_label(settings.effort.as_deref().unwrap_or(&m.default_effort)))
        .unwrap_or_default();
    if settings.fast && current.is_some_and(|m| m.fast.is_some()) {
        detail = if detail.is_empty() {
            "Fast".to_owned()
        } else {
            format!("{detail} · Fast")
        };
    }
    // a narrow composer keeps the effort and drops the name
    let full = Chip::new(name)
        .detail(&detail)
        .provider(settings.provider)
        .width(ui);
    let chip = Chip::new(if ui.available_width() > full + 160.0 {
        name
    } else {
        ""
    })
    .detail(&detail)
    .provider(settings.provider)
    .show(ui)
    .tip(format!("Model: {name} {detail}"));

    let popup_id = egui::Popup::default_response_id(&chip);
    let state_id = chip.id.with("picker");
    if !egui::Popup::is_id_open(ui.ctx(), popup_id) {
        ui.data_mut(|d| {
            d.remove_temp::<String>(state_id.with("search"));
            d.remove_temp::<Provider>(state_id.with("viewing"));
            d.remove_temp::<bool>(state_id.with("access"));
        });
    }

    let mut next = None;
    egui::Popup::menu(&chip)
        .align(egui::RectAlign::TOP_END)
        .gap(space::SM)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .frame(theme::floating(ui).inner_margin(0))
        .show(|ui| {
            next = panel(ui, state_id, models, settings, locked);
            if next.is_some() {
                ui.close();
            }
        });
    next
}

fn pick<'a>(
    models: &'a Models,
    settings: &AgentSettings,
    provider: Provider,
) -> Option<&'a ModelOption> {
    let list = models.get(&provider)?;
    settings
        .model
        .as_deref()
        .filter(|_| provider == settings.provider)
        .and_then(|id| list.iter().find(|m| m.id == id))
        .or_else(|| list.iter().find(|m| m.is_default))
        .or(list.first())
}

fn panel(
    ui: &mut egui::Ui,
    state_id: egui::Id,
    models: &Models,
    settings: &AgentSettings,
    locked: bool,
) -> Option<AgentSettings> {
    let p = theme::palette(ui);
    let size = vec2(RAIL + LIST + SIDE, BODY + BAR);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let rail = Rect::from_min_size(rect.min, vec2(RAIL, BODY));
    let list = Rect::from_min_size(pos2(rail.right(), rect.top()), vec2(LIST, BODY));
    let side = Rect::from_min_size(pos2(list.right(), rect.top()), vec2(SIDE, BODY));
    let bar = Rect::from_min_size(pos2(rect.left(), rect.top() + BODY), vec2(size.x, BAR));
    let line = Stroke::new(1.0, p.border);
    ui.painter().vline(rail.right(), rail.y_range(), line);
    ui.painter().vline(side.left(), side.y_range(), line);
    ui.painter().hline(bar.x_range(), bar.top(), line);

    let mut next = None;
    let viewing_id = state_id.with("viewing");
    let mut viewing = ui
        .data(|d| d.get_temp::<Provider>(viewing_id))
        .unwrap_or(settings.provider);

    // the agents down the side, the one being looked at marked with a bar like t3 code's
    let mut tabs = Vec::new();
    for (i, provider) in Provider::ALL.into_iter().enumerate() {
        let at = Rect::from_center_size(
            pos2(
                rail.center().x,
                rail.top() + space::SM + 22.0 + i as f32 * 48.0,
            ),
            vec2(40.0, 40.0),
        );
        let tab = ui
            .interact(at, state_id.with(("tab", provider)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .tip(provider.label());
        if tab.clicked() {
            viewing = provider;
        }
        let active = viewing == provider;
        if tab.hovered() && !active {
            ui.painter().rect_filled(
                at,
                CornerRadius::same(radius::MD),
                theme::tint(p, theme::fill::HOVER),
            );
        }
        let color = if active { p.text } else { p.text_muted };
        provider_mark(
            ui.painter(),
            at.center(),
            icon::LG + 4.0,
            provider,
            provider_color(provider, color).gamma_multiply(if active { 1.0 } else { 0.7 }),
        );
        tabs.push(at);
    }
    let index = Provider::ALL
        .iter()
        .position(|p| *p == viewing)
        .unwrap_or(0);
    let y = motion::spring(ui.ctx(), state_id.with("bar"), index as f32, Spring::SNAPPY);
    let mark = tabs[0].translate(vec2(0.0, (tabs[1].top() - tabs[0].top()) * y));
    ui.painter().rect_filled(
        Rect::from_min_max(
            pos2(rail.right() - 4.0, mark.top() + 8.0),
            pos2(rail.right() - 1.0, mark.bottom() - 8.0),
        ),
        CornerRadius::same(2),
        p.accent,
    );
    ui.data_mut(|d| d.insert_temp(viewing_id, viewing));

    // the search box over the list, looking through every agent's models
    let search_id = state_id.with("search");
    let mut search = ui
        .data(|d| d.get_temp::<String>(search_id))
        .unwrap_or_default();
    let header = Rect::from_min_size(list.min, vec2(LIST, HEADER));
    ui.painter().text(
        pos2(
            header.left() + space::MD + icon::MD / 2.0,
            header.center().y,
        ),
        Align2::CENTER_CENTER,
        icons::MAGNIFYING_GLASS,
        font::icon(icon::MD),
        p.text_faint,
    );
    let field_rect = Rect::from_min_max(
        pos2(
            header.left() + space::MD + icon::MD + space::SM,
            header.top(),
        ),
        pos2(header.right() - space::MD, header.bottom()),
    );
    let field_id = state_id.with("field");
    let field = ui.put(
        field_rect,
        egui::TextEdit::singleline(&mut search)
            .id(field_id)
            .frame(egui::Frame::NONE)
            .hint_text("Search models…")
            .font(font::regular(text::MD + 1.0))
            .align(egui::Align2::LEFT_CENTER)
            .margin(egui::Margin::ZERO),
    );
    if !ui.memory(|m| m.has_focus(field_id)) && !field.lost_focus() {
        field.request_focus();
    }
    ui.painter().hline(
        (header.left() + space::MD)..=(header.right() - space::MD),
        header.bottom(),
        line,
    );
    ui.data_mut(|d| d.insert_temp(search_id, search.clone()));

    let query = search.trim().to_lowercase();
    let rows: Vec<(Provider, &ModelOption)> = if query.is_empty() {
        models
            .get(&viewing)
            .map(|list| list.iter().map(|m| (viewing, m)).collect())
            .unwrap_or_default()
    } else {
        Provider::ALL
            .into_iter()
            .flat_map(|provider| {
                models
                    .get(&provider)
                    .into_iter()
                    .flatten()
                    .map(move |m| (provider, m))
            })
            .filter(|(_, m)| m.name.to_lowercase().contains(&query))
            .collect()
    };
    let body = Rect::from_min_max(pos2(list.left(), header.bottom() + 1.0), list.max);
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body.shrink2(vec2(space::SM, space::XS)))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    // a chat already running on one agent stays on it
    if locked && query.is_empty() && viewing != settings.provider {
        note(
            &mut content,
            viewing,
            &format!("This chat runs on {}", settings.provider.label()),
            &format!("Start a new agent to use {}", viewing.label()),
        );
    } else if query.is_empty() && !models.contains_key(&viewing) {
        note(
            &mut content,
            viewing,
            &format!("Loading {} models…", viewing.label()),
            "",
        );
    } else if rows.is_empty() {
        note(&mut content, viewing, "No models match", "");
    } else {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(&mut content, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (provider, model) in rows {
                    let blocked = locked && provider != settings.provider;
                    let chosen = provider == settings.provider
                        && pick(models, settings, provider).is_some_and(|c| c.id == model.id);
                    let row = model_row(ui, provider, model, chosen, blocked);
                    if row.clicked() && !blocked && !chosen {
                        let mut changed = settings.clone();
                        // effort and speed carry over only when the new model supports them
                        if changed.provider != provider
                            || changed
                                .effort
                                .as_ref()
                                .is_some_and(|e| !model.efforts.contains(e))
                        {
                            changed.effort = None;
                        }
                        changed.fast &= model.fast.is_some() && changed.provider == provider;
                        changed.provider = provider;
                        changed.model = Some(model.id.clone());
                        next = Some(changed);
                    }
                }
            });
    }

    // the current model's reasoning, picked beside the list
    let current = pick(models, settings, settings.provider);
    let side_header = Rect::from_min_size(side.min, vec2(SIDE, HEADER));
    ui.painter().text(
        pos2(side_header.left() + space::MD + 2.0, side_header.center().y),
        Align2::LEFT_CENTER,
        "Reasoning",
        font::regular(text::MD + 1.0),
        p.text_muted,
    );
    if let Some(model) = current {
        let mut job = egui::text::LayoutJob::simple_singleline(
            short_name(&model.name).to_owned(),
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(SIDE / 2.0);
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            pos2(
                side_header.right() - space::MD - galley.size().x,
                side_header.center().y - galley.size().y / 2.0,
            ),
            galley,
            p.text_faint,
        );
    }
    ui.painter().hline(
        (side.left() + space::MD)..=(side.right() - space::MD),
        side_header.bottom(),
        line,
    );
    let mut y = side_header.bottom() + space::XS;
    match current.filter(|m| !m.efforts.is_empty()) {
        Some(model) => {
            let chosen = settings
                .effort
                .clone()
                .unwrap_or_else(|| model.default_effort.clone());
            for option in &model.efforts {
                let at = Rect::from_min_size(
                    pos2(side.left() + space::SM, y),
                    vec2(SIDE - space::SM * 2.0, ROW),
                );
                y += ROW;
                if choice_row(
                    ui,
                    at,
                    &effort_label(option),
                    *option == chosen,
                    state_id.with(("effort", option)),
                )
                .clicked()
                    && *option != chosen
                {
                    let mut changed = settings.clone();
                    changed.effort = Some(option.clone());
                    next = Some(changed);
                }
            }
        }
        None => {
            ui.painter().text(
                pos2(side.left() + space::MD + 2.0, y + ROW / 2.0),
                Align2::LEFT_CENTER,
                "This model has no settings",
                font::regular(text::SM + 1.0),
                p.text_faint,
            );
        }
    }

    // speed on the left of the bar and permissions on the right
    let inner = bar.shrink2(vec2(space::MD, 0.0));
    if let Some(model) = current.filter(|m| m.fast.is_some()) {
        let mut fast = settings.fast;
        let mut speed = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let segmented = Segmented::new(&mut fast, &[(false, "Standard"), (true, "Fast")])
            .show(&mut speed)
            .tip(model.fast.as_deref().unwrap_or_default());
        if segmented.changed() {
            let mut changed = settings.clone();
            changed.fast = fast;
            next = Some(changed);
        }
    }
    if let Some(access) = permissions(ui, state_id, inner, rect, settings.access) {
        let mut changed = settings.clone();
        changed.access = access;
        next = Some(changed);
    }
    next
}

fn model_row(
    ui: &mut egui::Ui,
    provider: Provider,
    model: &ModelOption,
    chosen: bool,
    blocked: bool,
) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let response = if blocked {
        response.tip(format!("Start a new agent to use {}", provider.label()))
    } else {
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .tip(&model.description)
    };
    if response.hovered() && !blocked {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius::MD),
            theme::tint(p, theme::fill::HOVER),
        );
    }
    let color = if blocked { p.text_faint } else { p.text };
    provider_mark(
        ui.painter(),
        pos2(rect.left() + space::SM + icon::MD / 2.0, rect.center().y),
        icon::MD + 2.0,
        provider,
        provider_color(provider, p.text_muted).gamma_multiply(if blocked { 0.5 } else { 1.0 }),
    );
    let name = ui.painter().text(
        pos2(
            rect.left() + space::SM + icon::MD + space::MD,
            rect.center().y,
        ),
        Align2::LEFT_CENTER,
        &model.name,
        font::regular(text::MD + 1.0),
        color,
    );
    if chosen {
        ui.painter().text(
            pos2(name.right() + space::MD, rect.center().y),
            Align2::LEFT_CENTER,
            icons::CHECK,
            font::icon(icon::MD),
            p.text_muted,
        );
    }
    response
}

fn choice_row(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    chosen: bool,
    id: egui::Id,
) -> egui::Response {
    let p = theme::palette(ui);
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius::MD),
            theme::tint(p, theme::fill::HOVER),
        );
    }
    ui.painter().text(
        pos2(rect.left() + space::SM, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font::regular(text::MD + 1.0),
        p.text,
    );
    if chosen {
        ui.painter().text(
            pos2(rect.right() - space::SM, rect.center().y),
            Align2::RIGHT_CENTER,
            icons::CHECK,
            font::icon(icon::MD),
            p.text_muted,
        );
    }
    response
}

// the permission button on the bar, and the card it opens over the panel's corner
fn permissions(
    ui: &mut egui::Ui,
    state_id: egui::Id,
    bar: Rect,
    panel: Rect,
    access: Access,
) -> Option<Access> {
    let p = theme::palette(ui);
    let open_id = state_id.with("access");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let label = ui.painter().layout_no_wrap(
        access.label().to_owned(),
        font::regular(text::MD + 1.0),
        p.text,
    );
    let width =
        space::SM + icon::MD + space::SM + label.size().x + space::SM + icon::SM + space::SM;
    let button = Rect::from_min_size(
        pos2(bar.right() - width, bar.center().y - 16.0),
        vec2(width, 32.0),
    );
    let response = ui
        .interact(button, state_id.with("access-button"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Permissions"));
    if response.clicked() {
        open = !open;
    }
    let lit = if open {
        1.0
    } else {
        mux_ui::components::hover_t(ui, &response)
    };
    ui.painter().rect_filled(
        button,
        CornerRadius::same(radius::MD),
        theme::tint(p, theme::fill::HOVER * 1.6 * lit),
    );
    let mut x = button.left() + space::SM;
    ui.painter().text(
        pos2(x + icon::MD / 2.0, button.center().y),
        Align2::CENTER_CENTER,
        access_icon(access),
        font::icon(icon::MD),
        p.text_muted,
    );
    x += icon::MD + space::SM;
    ui.painter().galley(
        pos2(x, button.center().y - label.size().y / 2.0),
        label,
        p.text,
    );
    ui.painter().text(
        pos2(button.right() - space::SM, button.center().y),
        Align2::RIGHT_CENTER,
        if open {
            icons::CARET_DOWN
        } else {
            icons::CARET_UP
        },
        font::icon(icon::SM),
        p.text_faint,
    );

    let mut picked = None;
    if open {
        // drawn on the panel's own layer, so a click on it doesn't count as a click outside the panel
        let card = Rect::from_min_max(
            pos2(
                panel.right() - 300.0 - space::SM,
                button.top() - space::SM - 132.0,
            ),
            pos2(panel.right() - space::SM, button.top() - space::SM),
        );
        let blocker = ui.interact(card, state_id.with("access-card"), Sense::click());
        ui.painter().rect(
            card,
            CornerRadius::same(radius::LG),
            p.elevated,
            Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            pos2(card.left() + space::MD + 2.0, card.top() + 22.0),
            Align2::LEFT_CENTER,
            "Permissions",
            font::regular(text::SM + 1.0),
            p.text_muted,
        );
        let track = Rect::from_min_size(
            pos2(card.left() + space::MD, card.top() + 38.0),
            vec2(card.width() - space::MD * 2.0, 34.0),
        );
        ui.painter().rect_filled(
            track,
            CornerRadius::same(radius::MD),
            theme::tint(p, theme::fill::HOVER),
        );
        let segment = (track.width() - 4.0) / Access::ALL.len() as f32;
        let mut shown = access;
        for (i, option) in Access::ALL.into_iter().enumerate() {
            let at = Rect::from_min_size(
                pos2(track.left() + 2.0 + segment * i as f32, track.top() + 2.0),
                vec2(segment, track.height() - 4.0),
            );
            let r = ui
                .interact(at, state_id.with(("access", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if r.hovered() {
                shown = option;
            }
            if r.clicked() {
                picked = Some(option);
            }
            if option == access {
                ui.painter().rect_filled(
                    at,
                    CornerRadius::same(radius::MD),
                    theme::tint(p, theme::fill::SELECTED),
                );
            }
            ui.painter().text(
                at.center(),
                Align2::CENTER_CENTER,
                access_icon(option),
                font::icon(icon::MD + 2.0),
                if option == access || r.hovered() {
                    p.text
                } else {
                    p.text_muted
                },
            );
        }
        ui.painter().text(
            pos2(card.left() + space::MD + 2.0, track.bottom() + 20.0),
            Align2::LEFT_CENTER,
            shown.label(),
            font::medium(text::MD + 1.0),
            p.text,
        );
        ui.painter().text(
            pos2(card.left() + space::MD + 2.0, track.bottom() + 40.0),
            Align2::LEFT_CENTER,
            shown.description(),
            font::regular(text::SM + 1.0),
            p.text_muted,
        );
        // a click anywhere else in the panel puts the card away
        let clicked = ui.input(|i| i.pointer.any_click());
        if picked.is_some()
            || (clicked && !blocker.contains_pointer() && !response.contains_pointer())
        {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    picked.filter(|option| *option != access)
}

fn note(ui: &mut egui::Ui, provider: Provider, title: &str, detail: &str) {
    let p = theme::palette(ui);
    let rect = ui.available_rect_before_wrap();
    let center = rect.center() - vec2(0.0, 20.0);
    let lines = if detail.is_empty() { 0.0 } else { 18.0 };
    let mark = pos2(center.x, center.y - 14.0 - lines / 2.0);
    provider_mark(
        ui.painter(),
        mark,
        icon::LG + 4.0,
        provider,
        provider_color(provider, p.text_faint).gamma_multiply(0.7),
    );
    ui.painter().text(
        pos2(center.x, mark.y + 24.0),
        Align2::CENTER_CENTER,
        title,
        font::medium(text::MD),
        p.text,
    );
    if !detail.is_empty() {
        ui.painter().text(
            pos2(center.x, mark.y + 24.0 + lines),
            Align2::CENTER_CENTER,
            detail,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
    }
}

// "Default (Opus 5.5)" reads as "Opus 5.5" in the reasoning header
fn short_name(name: &str) -> &str {
    name.rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or(name)
}

pub fn access_icon(access: Access) -> char {
    match access {
        Access::Supervised => icons::LOCK_SIMPLE,
        Access::AutoEdits => icons::PENCIL_SIMPLE_LINE,
        Access::Auto => icons::SPARKLE,
        Access::Full => icons::SHIELD_SLASH,
    }
}

pub fn effort_label(effort: &str) -> String {
    match effort {
        "" => "Auto".to_owned(),
        "xhigh" => "Extra high".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}
