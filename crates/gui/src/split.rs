use super::*;

impl App {
    // a terminal on the right beside the session, returning what's left of `main` for it
    pub(crate) fn split_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        let p = theme::palette(ui);
        if let Some(id) = self.sessions.split {
            self.split_shown = Some(id);
        }
        let open = self.sessions.split.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("split-panel"), open, Spring::QUICK)
            .clamp(0.0, 1.0);
        let Some(id) = self.split_shown.filter(|_| open || t > 0.01) else {
            self.split_shown = None;
            self.split_focused = None;
            return main;
        };
        let full = self
            .split_width
            .clamp(280.0, (main.width() * 0.7).max(280.0));
        // the chat takes its new width once, at the start of opening and the end of closing,
        // while the pane fades and eases in over that space, so nothing reflows mid animation
        let edge = main.right() - full;
        let handle = Rect::from_min_max(
            pos2(edge - 3.0, main.top()),
            pos2(edge + 3.0, main.bottom()),
        );
        if open {
            let drag = ui
                .interact(
                    handle,
                    egui::Id::unique("split-resize"),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if drag.dragged() {
                self.split_width = full - drag.drag_delta().x;
            }
        }
        let slide = 24.0 * (1.0 - t);
        let panel = Rect::from_min_size(pos2(edge + slide, main.top()), vec2(full, main.height()));
        let clip = Rect::from_min_max(pos2(edge, main.top()), main.max).intersect(ui.clip_rect());
        let mut pane = ui.new_child(UiBuilder::new().id_salt("split-pane").max_rect(panel));
        pane.set_clip_rect(clip);
        pane.multiply_opacity(t);
        // terminal text needs a steady backdrop, so it never shows the blur behind the window
        pane.painter().rect_filled(panel, 0.0, p.bg);
        pane.painter().vline(
            panel.left(),
            panel.y_range(),
            egui::Stroke::new(1.0, p.border),
        );

        let header = Rect::from_min_size(panel.min, vec2(full, FOOTER_ROW + space::SM));
        let mut close = false;
        let codex_reset = self.sessions.reset_offer(Provider::Codex);
        let Some((info, local, outbox, models, folder)) = self.sessions.split_session(id) else {
            self.split_shown = None;
            return main;
        };
        let mut head = pane.new_child(
            UiBuilder::new()
                .id_salt("split-header")
                .max_rect(header.shrink2(vec2(space::MD, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let agent = info.kind == SessionKind::Agent;
        if agent {
            let (mark, _) =
                head.allocate_exact_size(vec2(icon::MD, icon::MD), egui::Sense::hover());
            provider_mark(
                head.painter(),
                mark.center(),
                icon::MD,
                info.provider,
                provider_color(info.provider, p.text_muted),
            );
        } else {
            let glyph = match info.kind {
                SessionKind::Task => icons::PLAY,
                _ => icons::TERMINAL,
            };
            head.label(
                egui::RichText::new(glyph.to_string())
                    .font(font::icon(icon::MD))
                    .color(p.text_faint),
            );
        }
        head.label(
            egui::RichText::new(&info.title)
                .font(font::medium(text::MD))
                .color(p.text_muted),
        );
        let ended = sessions::ended(info.status);
        // a shell that exits takes its pane with it, the way it closes a terminal tab
        if ended && info.kind == SessionKind::Shell {
            close = true;
        }
        let mut restart = false;
        head.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close |= IconButton::new(icons::X, "Close")
                .show(ui)
                .tip_with_shortcut("Close the pane", TERMINAL)
                .clicked();
            restart = ended
                && !agent
                && IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                    .show(ui)
                    .tip("Run it again")
                    .clicked();
        });
        pane.painter().hline(
            header.x_range(),
            header.bottom(),
            egui::Stroke::new(1.0, p.border),
        );

        if agent {
            let body = Rect::from_min_max(pos2(panel.left(), header.bottom()), panel.max);
            let mut body_ui = pane.new_child(UiBuilder::new().id_salt("split-body").max_rect(body));
            let reset = codex_reset.filter(|_| local.settings.provider == Provider::Codex);
            let action = agent_chat(
                &mut body_ui,
                AgentPane {
                    id,
                    reset,
                    local,
                    outbox,
                    models,
                    folder: folder.as_deref(),
                },
                false,
                Vec::new(),
                false,
            );
            match action {
                Some(ChatAction::OpenLink(target)) => {
                    self.open_link(&ctx, &target, folder.as_deref())
                }
                Some(ChatAction::UseReset) => self.sessions.use_reset(Provider::Codex),
                Some(ChatAction::OpenDiff(index)) => {
                    let chat = self.sessions.split_session(id).map(|s| &s.1.chat);
                    if let Some(chat) = chat {
                        self.diffs = changed_files(chat, folder.as_deref());
                        let clicked = match chat.get(index) {
                            Some(ChatItem::Diff(diff)) => {
                                Some(resolve(&diff.path, folder.as_deref()))
                            }
                            _ => None,
                        };
                        if let Some(at) =
                            clicked.and_then(|p| self.diffs.iter().position(|d| d.0 == p))
                        {
                            self.show_diff(at);
                        }
                    }
                }
                _ => {}
            }
            if close {
                self.sessions.split = None;
            }
            return Rect::from_min_max(main.min, pos2(edge, main.bottom()));
        }
        // attaching starts the process on the server if it has not run yet, then streams it here
        if local.terminal.is_none() && !local.attaching {
            outbox.push(ClientMsg::Attach { id });
            local.attaching = true;
        }
        if let Some(terminal) = &mut local.terminal {
            let body = Rect::from_min_max(pos2(panel.left(), header.bottom()), panel.max)
                .shrink2(vec2(space::MD, space::SM));
            let mut body_ui = pane.new_child(UiBuilder::new().id_salt("split-body").max_rect(body));
            let (response, zoom) =
                terminal.show(&mut body_ui, id, outbox, self.prefs.terminal_size());
            if let Some(size) = zoom {
                zoom_terminal(&mut self.prefs, size);
            }
            // a terminal just opened on the right takes the keyboard, as if it had been clicked
            if open && self.split_focused != Some(id) {
                response.request_focus();
                self.split_focused = Some(id);
            }
        }
        if restart {
            self.sessions.restart(id);
        }
        if close {
            self.sessions.split = None;
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }

    // the file viewer on the right, returning what's left of `main` for the session
    pub(crate) fn file_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        if self.viewer_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.viewer_open = false;
        }
        let open = self.viewer_open && self.viewer.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("file-panel"), open, Spring::SMOOTH)
            .clamp(0.0, 1.0);
        if t <= 0.001 {
            if !open {
                self.viewer = None;
            }
            return main;
        }
        let full = self
            .viewer_width
            .clamp(320.0, (main.width() * 0.7).max(320.0));
        // the session takes its final width at once and the panel slides over that edge,
        // so the chat rewraps a single time instead of on every frame of the slide
        let left = main.right() - full * t;
        let edge = if open { main.right() - full } else { left };
        let handle = Rect::from_min_max(
            pos2(left - 3.0, main.top()),
            pos2(left + 3.0, main.bottom()),
        );
        let drag = ui
            .interact(
                handle,
                egui::Id::unique("file-panel-resize"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if drag.dragged() {
            self.viewer_width = full - drag.drag_delta().x;
        }
        // laid out at full width and slid in from the edge, so the file doesn't reflow while it opens
        let panel = Rect::from_min_size(pos2(left, main.top()), vec2(full, main.height()));
        let mut panel_ui = ui.new_child(UiBuilder::new().id_salt("file-panel").max_rect(panel));
        panel_ui.set_clip_rect(panel.intersect(main).intersect(ui.clip_rect()));
        let action = self.viewer.as_mut().and_then(|v| v.show(&mut panel_ui));
        if let (Some(action), Some(view)) = (action, &self.viewer) {
            match action {
                FileViewAction::Close => self.viewer_open = false,
                FileViewAction::ToggleWrap => {
                    if let Some(view) = &mut self.viewer {
                        view.wrap = !view.wrap;
                    }
                }
                FileViewAction::CopyPath => ctx.copy_text(view.path.clone()),
                FileViewAction::OpenInEditor => open_in_editor(&view.path, view.editor_line()),
                FileViewAction::Step(by) => {
                    let count = self.diffs.len() as isize;
                    if count > 0 {
                        let at = (self.diff_index as isize + by).rem_euclid(count);
                        self.show_diff(at as usize);
                    }
                }
            }
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }
}
