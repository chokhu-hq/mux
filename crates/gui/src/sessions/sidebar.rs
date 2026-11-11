use super::*;

impl Sessions {
    pub(super) fn is_noop(&self, id: u64, drop: Drop) -> bool {
        self.locate(id)
            .is_some_and(|(g, i)| g == drop.group && (drop.index == i || drop.index == i + 1))
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let dragging = self.drag.as_ref().map(|d| d.id);
        let group_dragging = self.group_drag.as_ref().map(|d| d.id);
        let pointer = ui.ctx().pointer_latest_pos();
        let mut actions = Vec::new();
        let mut drop: Option<(Drop, Option<f32>)> = None;
        let width = ui.available_width();
        // where each group's header and rows sit, to find the gap a dragged group would land in
        let mut blocks = Vec::new();

        ui.spacing_mut().item_spacing.y = 0.0;
        for (g, group) in self.groups.iter().enumerate() {
            let block_top = ui.cursor().top();
            let opacity = ui.opacity();
            if group_dragging == Some(group.id) {
                ui.multiply_opacity(0.35);
            }
            if let Some((id, draft)) = &mut self.renaming
                && *id == group.id
            {
                let shown = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name)
                            .open(group.open)
                            .rename(draft)
                            .show(ui)
                    })
                    .inner;
                if let Some(input) = shown.field {
                    if !input.has_focus() && !input.lost_focus() {
                        input.request_focus();
                        select_all(ui.ctx(), input.id, draft);
                    }
                    let (enter, escape) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                    if input.lost_focus() {
                        if enter && !escape {
                            actions.push(Action::Rename(group.id, std::mem::take(draft)));
                        }
                        actions.push(Action::EndRename);
                    }
                }
            } else {
                let shown = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name)
                            // a closed project still says when something in it needs looking at
                            .status(
                                (!group.open)
                                    .then(|| most_pressing(&group.sessions, &self.unread))
                                    .flatten(),
                            )
                            .detail(if group.path.is_some() {
                                ""
                            } else {
                                "no folder"
                            })
                            .open(group.open)
                            .drop_target(dragging.is_some())
                            .show(ui)
                    })
                    .inner;
                if shown.add.tip("New agent in this project").clicked() {
                    actions.push(Action::NewAgent(group.id));
                }
                let folder = group
                    .path
                    .as_deref()
                    .map(mux_core::display_path)
                    .unwrap_or_else(|| "No folder yet, right-click to choose one".to_owned());
                let header = shown.response.tip(folder);
                if header.clicked() {
                    actions.push(Action::Toggle(group.id));
                }
                if header.drag_started()
                    && dragging.is_none()
                    && let Some(pos) = header.interact_pointer_pos()
                {
                    actions.push(Action::StartGroupDrag(Drag {
                        id: group.id,
                        grab: pos - header.rect.min,
                        width: header.rect.width(),
                    }));
                }
                header.context_menu(|ui| {
                    if MenuItem::new("New agent")
                        .icon(icons::ROBOT)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::NewAgent(group.id));
                        ui.close();
                    }
                    if MenuItem::new("New shell")
                        .icon(icons::TERMINAL_WINDOW)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::NewShell(group.id));
                        ui.close();
                    }
                    folder_items(ui, group.path.as_deref());
                    if MenuItem::new("Rename")
                        .icon(icons::PENCIL_SIMPLE)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::StartRename(group.id, group.name.clone()));
                        ui.close();
                    }
                    if MenuItem::new("Change folder…")
                        .icon(icons::FOLDER_OPEN)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::ChangeFolder(group.id));
                        ui.close();
                    }
                    if group.sessions.is_empty()
                        && MenuItem::new("Remove project")
                            .icon(icons::TRASH)
                            .danger()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Action::RemoveGroup(group.id));
                        ui.close();
                    }
                });
                if dragging.is_some() && header.contains_pointer() {
                    let index = if group.open { 0 } else { group.sessions.len() };
                    drop = Some((Drop { group: g, index }, None));
                }
            }

            let n = group.sessions.len() as f32;
            let full = n * (SessionRow::HEIGHT + ROW_GAP);
            let t = spring_bool(
                ui.ctx(),
                Id::unique("mux-group").with(group.id),
                group.open,
                Spring::SMOOTH,
            )
            .max(0.0);
            let (slot, _) = ui.allocate_exact_size(vec2(width, full * t), Sense::hover());
            if slot.height() > 0.5 {
                let mut rows = ui.new_child(
                    UiBuilder::new().max_rect(Rect::from_min_size(slot.min, vec2(width, full))),
                );
                rows.set_clip_rect(slot.intersect(ui.clip_rect()));
                rows.multiply_opacity(t.min(1.0));
                rows.spacing_mut().item_spacing.y = 0.0;
                for (i, session) in group.sessions.iter().enumerate() {
                    rows.add_space(ROW_GAP);
                    let is_dragged = dragging == Some(session.id);
                    // a row being renamed keeps everything but its title, which becomes the field
                    let renaming = self
                        .renaming
                        .as_mut()
                        .filter(|(id, _)| *id == session.id)
                        .map(|(_, draft)| draft);
                    let (row, field) = rows
                        .push_id(session.id, |ui| {
                            if is_dragged {
                                ui.multiply_opacity(0.35);
                            }
                            let (context, time) = (row_context(session), ago(session.updated));
                            let mut row =
                                SessionRow::new(&session.title, session.kind, session.status)
                                    .context(&context)
                                    .time(&time)
                                    .branch(
                                        session
                                            .branch
                                            .as_deref()
                                            .or(group.branch.as_deref())
                                            .unwrap_or_default(),
                                    )
                                    .selected(self.selected == Some(session.id))
                                    .unread(self.unread.contains(&session.id));
                            let model = model_name(&self.models, session);
                            if session.kind == SessionKind::Agent {
                                row = row.provider(session.provider).model(&model);
                            }
                            if let Some(summary) = &session.summary {
                                row = row.changes(summary.added, summary.removed);
                            }
                            if let Some(draft) = renaming {
                                row = row.rename(draft);
                            }
                            row.show_with_field(ui)
                        })
                        .inner;
                    if let Some(input) = field {
                        if !input.has_focus() && !input.lost_focus() {
                            input.request_focus();
                            if let Some((_, draft)) = &self.renaming {
                                select_all(rows.ctx(), input.id, draft);
                            }
                        }
                        let (enter, escape) =
                            rows.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        if input.lost_focus() {
                            if enter
                                && !escape
                                && let Some((_, draft)) = &mut self.renaming
                            {
                                actions.push(Action::Rename(session.id, std::mem::take(draft)));
                            }
                            actions.push(Action::EndRename);
                        }
                        continue;
                    }
                    if row.clicked() {
                        actions.push(Action::Select(session.id));
                    }
                    if row.double_clicked() {
                        actions.push(Action::StartRename(session.id, session.title.clone()));
                    }
                    if row.drag_started()
                        && let Some(pos) = row.interact_pointer_pos()
                    {
                        actions.push(Action::StartDrag(Drag {
                            id: session.id,
                            grab: pos - row.rect.min,
                            width: row.rect.width(),
                        }));
                    }
                    row.context_menu(|ui| {
                        folder_items(ui, session.worktree.as_deref().or(group.path.as_deref()));
                        if let Some(branch) = session.branch.as_ref().or(group.branch.as_ref()) {
                            mux_ui::copy_item(ui, "Copy branch name", branch);
                        }
                        if MenuItem::new("Rename")
                            .icon(icons::PENCIL_SIMPLE)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::StartRename(session.id, session.title.clone()));
                            ui.close();
                        }
                        if self.selected != Some(session.id)
                            && MenuItem::new("Open on the right")
                                .icon(icons::SIDEBAR_SIMPLE)
                                .show(ui)
                                .clicked()
                        {
                            actions.push(Action::Split(session.id));
                            ui.close();
                        }
                        if MenuItem::new("Delete")
                            .icon(icons::TRASH)
                            .danger()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::Remove(session.id));
                            ui.close();
                        }
                    });

                    let zone = row.rect.expand2(vec2(0.0, ROW_GAP / 2.0));
                    if dragging.is_some()
                        && let Some(pos) = pointer
                        && zone.contains(pos)
                        && slot.contains(pos)
                    {
                        let before = pos.y < row.rect.center().y;
                        let index = if before { i } else { i + 1 };
                        let y = if before { zone.top() } else { zone.bottom() };
                        drop = Some((Drop { group: g, index }, Some(y)));
                    }
                }
            }
            blocks.push((block_top, ui.cursor().top()));
            ui.set_opacity(opacity);
            ui.add_space(GROUP_GAP);
        }

        if let Some(id) = group_dragging {
            self.drop_group(ui, id, &blocks, pointer);
        }

        if let Some(id) = dragging {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            let drop = drop.filter(|(d, _)| !self.is_noop(id, *d));
            if let Some((_, Some(y))) = drop {
                let x = ui.max_rect().x_range();
                ui.painter()
                    .hline(x.min + 4.0..=x.max - 4.0, y, Stroke::new(2.0, p.accent));
                ui.painter()
                    .circle(pos2(x.min + 4.0, y), 3.0, p.bg, Stroke::new(2.0, p.accent));
            }
            self.ghost(ui, pointer);

            let (released, cancelled) = ui.input_mut(|i| {
                (
                    i.pointer.any_released(),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if released || cancelled {
                if let (false, Some((d, _))) = (cancelled, drop) {
                    self.outbox.push(ClientMsg::MoveSession {
                        id,
                        group: self.groups[d.group].id,
                        index: d.index,
                    });
                }
                self.drag = None;
            }
        }

        for action in actions {
            match action {
                Action::Toggle(id) => self.outbox.push(ClientMsg::ToggleGroup { id }),
                Action::NewAgent(group) => {
                    self.request(Some(group), "New agent", SessionKind::Agent, false)
                }
                Action::Split(id) => self.split = Some(id),
                Action::NewShell(group) => {
                    self.request(Some(group), "Shell", SessionKind::Shell, false)
                }
                Action::StartRename(id, name) => self.renaming = Some((id, name)),
                // groups and sessions share one run of ids, so the id says which is being renamed
                Action::Rename(id, name) => {
                    let group = self.groups.iter().any(|g| g.id == id);
                    self.outbox.push(if group {
                        ClientMsg::RenameGroup { id, name }
                    } else {
                        ClientMsg::RenameSession { id, title: name }
                    });
                }
                Action::EndRename => self.renaming = None,
                Action::ChangeFolder(id) => self.set_group_folder(id),
                Action::RemoveGroup(id) => self.outbox.push(ClientMsg::DeleteGroup { id }),
                Action::Select(id) => self.select(id),
                Action::Remove(id) => self.remove(id),
                Action::StartDrag(drag) => self.drag = Some(drag),
                Action::StartGroupDrag(drag) => self.group_drag = Some(drag),
            }
        }
    }

    // shows where a dragged group would land, and moves it there on release
    pub(super) fn drop_group(
        &mut self,
        ui: &egui::Ui,
        id: u64,
        blocks: &[(f32, f32)],
        pointer: Option<egui::Pos2>,
    ) {
        let p = theme::palette(ui);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let from = self.groups.iter().position(|g| g.id == id);
        let index = pointer.and_then(|pos| group_gap(blocks, pos.y, from));
        if let Some(index) = index {
            let y = match index {
                0 => blocks.first().map_or(0.0, |b| b.0) - GROUP_GAP / 2.0,
                i => blocks[i - 1].1 + GROUP_GAP / 2.0,
            };
            let x = ui.max_rect().x_range();
            ui.painter()
                .hline(x.min + 4.0..=x.max - 4.0, y, Stroke::new(2.0, p.accent));
            ui.painter()
                .circle(pos2(x.min + 4.0, y), 3.0, p.bg, Stroke::new(2.0, p.accent));
        }
        if let (Some(drag), Some(pos), Some(group)) = (
            &self.group_drag,
            pointer,
            self.groups.iter().find(|g| g.id == id),
        ) {
            egui::Area::new(Id::unique("mux-group-ghost"))
                .order(Order::Tooltip)
                .fixed_pos(pos - drag.grab)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    theme::floating(ui).show(ui, |ui| {
                        ui.set_width(drag.width);
                        GroupHeader::new(&group.name).open(group.open).show(ui);
                    });
                });
        }
        let (released, cancelled) = ui.input_mut(|i| {
            (
                i.pointer.any_released(),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if released || cancelled {
            if let (false, Some(index)) = (cancelled, index) {
                // moved here too so the sidebar doesn't jump back until the server agrees
                if let Some(from) = from {
                    let group = self.groups.remove(from);
                    let at = if from < index { index - 1 } else { index };
                    self.groups.insert(at.min(self.groups.len()), group);
                }
                self.outbox.push(ClientMsg::MoveGroup { id, index });
            }
            self.group_drag = None;
        }
    }

    // a lifted copy of the row that follows the pointer while dragging
    pub(super) fn ghost(&self, ui: &egui::Ui, pointer: Option<egui::Pos2>) {
        let (Some(drag), Some(pos)) = (&self.drag, pointer) else {
            return;
        };
        let Some(session) = self.infos().find(|s| s.id == drag.id) else {
            return;
        };
        egui::Area::new(Id::unique("mux-session-ghost"))
            .order(Order::Tooltip)
            .fixed_pos(pos - drag.grab)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                theme::floating(ui).show(ui, |ui| {
                    ui.set_width(drag.width);
                    SessionRow::new(&session.title, session.kind, session.status)
                        .context(&row_context(session))
                        .time(&ago(session.updated))
                        .show(ui);
                });
            });
    }
}

// the gap past every group whose middle is above the pointer, unless that leaves the group where it is
pub(super) fn group_gap(blocks: &[(f32, f32)], y: f32, from: Option<usize>) -> Option<usize> {
    let gap = blocks
        .iter()
        .filter(|(top, bottom)| (top + bottom) / 2.0 < y)
        .count();
    from.is_none_or(|from| gap != from && gap != from + 1)
        .then_some(gap)
}

// the folder a project or session works in, to copy or open in the file manager
pub(super) fn folder_items(ui: &mut egui::Ui, folder: Option<&str>) {
    let Some(folder) = folder else {
        return;
    };
    mux_ui::copy_item(ui, "Copy folder path", folder);
    if MenuItem::new("Open folder")
        .icon(icons::FOLDER_OPEN)
        .show(ui)
        .clicked()
    {
        mux_ui::chat::open_externally(&mux_core::paths::local(folder));
        ui.close();
    }
}

// the line above a row's title: what needs attention first, otherwise where it is
pub(super) fn row_context(session: &SessionInfo) -> String {
    match (session.status, session.kind) {
        (SessionStatus::Waiting, _) => "Needs your input".to_owned(),
        // an agent with a conversation that's simply not working right now has started, unlike a new one
        (SessionStatus::Idle, SessionKind::Agent)
            if session.summary.as_ref().is_some_and(|s| s.messages > 0) =>
        {
            "Idle".to_owned()
        }
        (SessionStatus::Idle, _) => "Not started".to_owned(),
        (SessionStatus::Running, SessionKind::Agent) => "Working…".to_owned(),
        (SessionStatus::Done, SessionKind::Agent) => "Finished".to_owned(),
        (SessionStatus::Failed, _) if session.meta.is_empty() => "Failed".to_owned(),
        _ => session.meta.clone(),
    }
}

pub(super) fn model_name(models: &Models, session: &SessionInfo) -> String {
    if session.kind != SessionKind::Agent {
        return String::new();
    }
    let list = models.get(&session.provider);
    let id = session
        .summary
        .as_ref()
        .map(|s| s.model.as_str())
        .unwrap_or_default();
    list.and_then(|list| {
        list.iter()
            .find(|m| !id.is_empty() && m.id == id)
            .or_else(|| list.iter().find(|m| id.is_empty() && m.is_default))
    })
    .map_or_else(|| id.to_owned(), |m| m.name.clone())
}

pub(super) fn ago(updated: u64) -> String {
    if updated == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(updated);
    match now.saturating_sub(updated) {
        s if s < 60 => "now".to_owned(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// a name being changed starts out selected, so typing replaces it the way renaming a file does,
// with the cursor at the start so a long name shows from its beginning rather than its end
pub(super) fn select_all(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            end,
            egui::text::CCursor::new(0),
        )));
    state.store(ctx, id);
}

// the status most worth knowing about among a project's sessions: one waiting on the person, then one that
// failed or finished since it was last looked at, with work in progress in between
pub(super) fn most_pressing(
    sessions: &[SessionInfo],
    unread: &HashSet<u64>,
) -> Option<SessionStatus> {
    sessions
        .iter()
        .filter_map(|session| {
            let unseen = unread.contains(&session.id);
            let rank = match session.status {
                SessionStatus::Waiting => 4,
                SessionStatus::Failed if unseen => 3,
                // a shell runs for as long as it's open, which is nothing to point out
                SessionStatus::Running if session.kind != SessionKind::Shell => 2,
                SessionStatus::Done if unseen => 1,
                _ => return None,
            };
            Some((rank, session.status))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, status)| status)
}
