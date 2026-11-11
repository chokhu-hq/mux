use super::*;

impl App {
    // window controls next to the traffic lights, then the session's breadcrumb and status over the content
    pub(crate) fn titlebar_contents(
        &mut self,
        ui: &mut egui::Ui,
        window: Rect,
        sidebar_width: f32,
        run: &mut Option<&'static str>,
    ) {
        let p = theme::palette(ui);
        let bar = Rect::from_min_size(window.min, vec2(window.width(), TITLEBAR_HEIGHT));
        let start = window.min.x + window::titlebar_leading_inset();
        let mut controls = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(start, bar.top()),
                    pos2(start + 60.0, bar.bottom()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        controls.spacing_mut().item_spacing.x = 2.0;
        if IconButton::new(icons::SIDEBAR_SIMPLE, "Toggle sidebar")
            .show(&mut controls)
            .tip_with_shortcut("Toggle sidebar", SIDEBAR_SHORTCUT)
            .clicked()
        {
            *run = Some("toggle-sidebar");
        }
        if IconButton::new(icons::FOLDER, "Add project")
            .show(&mut controls)
            .tip("Add project folder")
            .clicked()
        {
            *run = Some("new-group");
        }

        let Some(info) = self.titlebar.take() else {
            return;
        };
        let usage = info
            .provider
            .map(|provider| self.sessions.usage(provider).to_vec())
            .unwrap_or_default();
        let left =
            (window.min.x + sidebar_width + space::LG).max(controls.min_rect().right() + space::MD);
        let area = Rect::from_min_max(
            pos2(left, bar.top()),
            pos2(
                bar.right() - space::MD - window::titlebar_trailing_inset(),
                bar.bottom(),
            ),
        );
        // the status words give way first when the window is narrow, so the title keeps some room
        let status_width = ui
            .painter()
            .layout_no_wrap(
                info.status.clone(),
                font::regular(text::SM + 1.0),
                p.text_faint,
            )
            .size()
            .x;
        let show_status = area.width() - status_width > 520.0;
        // the actions on the right are laid out first, so the crumbs are cut short before them instead of running under
        let mut actions = ui.new_child(
            UiBuilder::new()
                .max_rect(area)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        actions.spacing_mut().item_spacing.x = space::SM;
        if IconButton::new(icons::TERMINAL_WINDOW, "Terminal")
            .selected(self.sessions.split.is_some())
            .show(&mut actions)
            .tip_with_shortcut("Terminal on the right", TERMINAL)
            .clicked()
        {
            *run = Some("toggle-terminal");
        }
        if info.restartable
            && IconButton::new(icons::ARROW_CLOCKWISE, "Restart")
                .show(&mut actions)
                .tip("Restart")
                .clicked()
        {
            self.sessions
                .outbox
                .push(ClientMsg::Restart { id: info.id });
        }
        if show_status {
            actions.label(
                egui::RichText::new(&info.status)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        }
        if let (Some(provider), false) = (info.provider, usage.is_empty()) {
            actions.add_space(space::SM);
            UsageRing::new(&usage)
                .provider(provider)
                .resets(self.sessions.resets_left(provider))
                .show(&mut actions);
        }
        if let Some((files, added, removed)) = info.changes {
            actions.add_space(space::SM);
            if changes_button(&mut actions, added, removed)
                .tip(format!(
                    "{files} {} changed in this chat, click to review",
                    if files == 1 { "file" } else { "files" }
                ))
                .clicked()
            {
                self.review = true;
            }
        }

        let right = (actions.min_rect().left() - space::LG).max(area.left());
        let mut crumbs = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(area.min, pos2(right, area.bottom())))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        crumbs.set_clip_rect(crumbs.max_rect().intersect(crumbs.clip_rect()));
        crumbs.spacing_mut().item_spacing.x = space::SM;
        if !info.project.is_empty() {
            // the project gives way to the session's own title, keeping only what the title leaves over
            let title = crumbs
                .painter()
                .layout_no_wrap(info.title.clone(), font::medium(text::MD), p.text)
                .size()
                .x;
            let extras = 3.0 * space::SM + icon::MD + 12.0;
            let room = crumbs.available_width();
            let max = (room - title - extras).max(room * 0.4).max(0.0);
            let mut job = egui::text::LayoutJob::simple_singleline(
                info.project.clone(),
                font::medium(text::MD),
                p.text_faint,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(max);
            let project = crumbs.painter().layout_job(job);
            let cut = project.elided;
            let response = crumbs.add(egui::Label::new(project));
            if cut {
                response.tip(&info.project);
            }
            crumbs.label(
                egui::RichText::new("›")
                    .font(font::regular(text::MD))
                    .color(p.text_faint),
            );
        }
        if let Some(provider) = info.provider {
            let (rect, _) =
                crumbs.allocate_exact_size(vec2(icon::MD, icon::MD), egui::Sense::hover());
            provider_mark(
                crumbs.painter(),
                rect.center(),
                icon::MD,
                provider,
                provider_color(provider, p.text_muted),
            );
        }
        crumbs.add(
            egui::Label::new(
                egui::RichText::new(&info.title)
                    .font(font::medium(text::MD))
                    .color(p.text),
            )
            .truncate(),
        );
        if let Some(branch) = &info.branch
            && crumbs.available_width() > 80.0
        {
            crumbs.add_space(space::XS);
            crumbs.spacing_mut().item_spacing.x = space::XS;
            crumbs.label(
                egui::RichText::new(icons::GIT_BRANCH.to_string())
                    .font(font::icon(icon::SM))
                    .color(p.text_faint),
            );
            crumbs.add(
                egui::Label::new(
                    egui::RichText::new(branch)
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_faint),
                )
                .truncate(),
            );
        }
    }
}
