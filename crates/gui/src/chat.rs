use super::*;

// the agent, model, effort and speed pickers in the composer, returning new settings when one changes
pub(crate) fn agent_controls(
    ui: &mut egui::Ui,
    settings: &AgentSettings,
    locked: bool,
) -> Option<AgentSettings> {
    let mut next = None;
    let menu = |response: &egui::Response| {
        egui::Popup::menu(response)
            .align(egui::RectAlign::TOP_START)
            .gap(space::XS)
    };
    let mode = if settings.plan { "Plan" } else { "Build" };
    let place = if settings.worktree {
        "Worktree"
    } else {
        "Local"
    };
    // a narrow composer drops the labels and keeps the icons
    let gap = ui.spacing().item_spacing.x;
    let place_width = if locked {
        0.0
    } else {
        Chip::new(place).icon(icons::FOLDER).width(ui) + space::SM + gap * 2.0
    };
    let mode_width = Chip::new(mode).icon(icons::ROBOT).toggle(false).width(ui) + gap;
    let show_mode = mode_width + place_width <= ui.available_width();

    // build or plan, a toggle since there are only the two
    let plan = Chip::new(if show_mode { mode } else { "" })
        .icon(if settings.plan {
            icons::LIST_CHECKS
        } else {
            icons::ROBOT
        })
        .toggle(settings.plan)
        .show(ui)
        .tip(if settings.plan {
            "Plan mode: the agent plans and waits for you. Click to build instead"
        } else {
            "Build mode: the agent makes changes. Click to plan first"
        });
    if plan.clicked() {
        let mut changed = settings.clone();
        changed.plan = !changed.plan;
        next = Some(changed);
    }

    // where it works can only be picked before the first message
    if !locked {
        control_separator(ui);
        let chip = Chip::new(place)
            .icon(if settings.worktree {
                icons::GIT_BRANCH
            } else {
                icons::FOLDER
            })
            .show(ui)
            .tip("Where the agent works");
        menu(&chip).show(|ui| {
            ui.set_width(300.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            menu_caption(ui, "Where it works");
            for (worktree, label, icon, detail) in [
                (
                    false,
                    "Local",
                    icons::FOLDER,
                    "In the project folder itself",
                ),
                (
                    true,
                    "New worktree",
                    icons::GIT_BRANCH,
                    "On its own branch, in a separate checkout",
                ),
            ] {
                if MenuItem::new(label)
                    .icon(icon)
                    .detail(detail)
                    .checked(settings.worktree == worktree)
                    .show(ui)
                    .clicked()
                {
                    let mut changed = settings.clone();
                    changed.worktree = worktree;
                    next = Some(changed);
                    ui.close();
                }
            }
        });
    }

    next
}

// what typing a slash offers: mux's commands that make sense right now, then the agent's own
pub(crate) fn slash_commands(
    provider: Provider,
    skills: &[mux_core::protocol::Skill],
    busy: bool,
    changed: bool,
) -> Vec<mux_ui::chat::SlashCommand> {
    let builtin = |name: &str, detail: &str| mux_ui::chat::SlashCommand {
        name: name.to_owned(),
        detail: detail.to_owned(),
        builtin: true,
    };
    let mut commands = vec![
        builtin("compact", "Summarise older messages to free up context"),
        builtin("plan", "Turn plan mode on or off"),
        builtin("new", "Start a new agent in this project"),
        builtin("mcp", "Add or change the MCP servers agents get"),
    ];
    if changed {
        commands.push(builtin(
            "review",
            "Step through the files this chat changed",
        ));
    }
    if busy {
        commands.push(builtin("stop", "Stop what the agent is doing"));
    }
    // a skill without a description of its own at least says where it comes from
    commands.extend(skills.iter().map(|skill| mux_ui::chat::SlashCommand {
        name: skill.name.clone(),
        detail: if skill.description.is_empty() {
            format!("{} {} skill", skill.scope, provider.label())
        } else {
            skill.description.clone()
        },
        builtin: false,
    }));
    commands
}

// a thin upright line between groups of composer controls
pub(crate) fn control_separator(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(space::SM, 16.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        egui::Stroke::new(1.0, p.border_strong),
    );
}

pub(crate) fn menu_caption(ui: &mut egui::Ui, label: &str) {
    let p = theme::palette(ui);
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new(label)
                .font(font::medium(text::SM))
                .color(p.text_faint),
        );
    });
    ui.add_space(space::XS);
}

// an agent session's chat on screen, wherever it's shown
pub(crate) struct AgentPane<'a> {
    pub(crate) id: u64,
    // a banked limit reset the chat's limit card can offer
    pub(crate) reset: Option<mux_ui::chat::ResetOffer>,
    pub(crate) local: &'a mut sessions::Local,
    pub(crate) outbox: &'a mut Vec<ClientMsg>,
    pub(crate) models: &'a sessions::Models,
    pub(crate) folder: Option<&'a str>,
}

// draws an agent's chat and carries out what only concerns that agent, handing back links, diffs and a new agent
pub(crate) fn agent_chat(
    ui: &mut egui::Ui,
    pane: AgentPane,
    dropping: bool,
    dropped: Vec<String>,
    review: bool,
) -> Option<ChatAction> {
    let AgentPane {
        id,
        reset,
        local,
        outbox,
        models,
        folder,
    } = pane;
    let first_diff = local
        .chat
        .iter()
        .position(|item| matches!(item, ChatItem::Diff(_)));
    if !local.opened {
        outbox.push(ClientMsg::AgentOpen { id });
        local.opened = true;
    }
    let settings = local.settings.clone();
    let started = local.has_started();
    let configure = std::cell::Cell::new(None);
    // the server only moves a chat to another agent between turns, with nothing left running in the background
    let switch_busy = local.busy
        || local
            .background
            .iter()
            .any(|task| task.state == wire::BackgroundState::Running);
    for path in dropped {
        if !local.attachments.contains(&path) {
            local.attachments.push(path);
        }
    }
    let commands = slash_commands(
        local.settings.provider,
        &local.skills,
        local.busy,
        first_diff.is_some(),
    );
    let action = ChatView::new(&local.chat, &mut local.draft)
        .session(id)
        .commands(&commands)
        .files(&local.files)
        .queue(&local.queue, local.held)
        .background(&local.background)
        .attachments(&mut local.attachments)
        .dropping(dropping)
        .running(local.busy)
        .reset(reset)
        .context(local.context)
        .controls(|ui| {
            if let Some(changed) = agent_controls(ui, &settings, started) {
                configure.set(Some(changed));
            }
        })
        .trailing(|ui| {
            if let Some(changed) = picker::show(ui, models, &settings, switch_busy) {
                configure.set(Some(changed));
            }
        })
        .show(ui);
    let action = action.or(first_diff.filter(|_| review).map(ChatAction::OpenDiff));
    // mux's own slash commands are the same actions the buttons around the chat take
    let action = match action {
        Some(ChatAction::Command(name)) => match name.as_str() {
            "compact" => Some(ChatAction::Compact),
            "review" => first_diff.map(ChatAction::OpenDiff),
            "stop" => Some(ChatAction::Stop),
            "plan" => {
                let mut settings = local.settings.clone();
                settings.plan = !settings.plan;
                local.settings = settings.clone();
                outbox.push(ClientMsg::AgentConfigure { id, settings });
                None
            }
            "new" | "mcp" => Some(ChatAction::Command(name)),
            _ => None,
        },
        action => action,
    };
    if let Some(settings) = configure.take() {
        local.settings = settings.clone();
        outbox.push(ClientMsg::AgentConfigure { id, settings });
    }
    match action {
        Some(ChatAction::Send(text)) => {
            // a file moved or deleted since it was attached is left out rather than failing the turn
            let mut attachments = std::mem::take(&mut local.attachments);
            attachments.retain(|path| std::path::Path::new(path).exists());
            // picked here, but the agent opens them where the server runs
            let attachments = attachments
                .iter()
                .map(|path| mux_core::paths::remote(path))
                .collect();
            outbox.push(ClientMsg::AgentSend {
                id,
                text,
                attachments,
            });
        }
        Some(ChatAction::Attach) => {
            let folder = folder.map(mux_core::paths::local);
            for path in sessions::pick_files(folder.as_deref()) {
                if !local.attachments.contains(&path) {
                    local.attachments.push(path);
                }
            }
        }
        // these reach beyond this one chat, so the caller carries them out
        action @ Some(ChatAction::OpenLink(_) | ChatAction::OpenDiff(_) | ChatAction::UseReset) => {
            return action;
        }
        Some(ChatAction::Stop) => outbox.push(ClientMsg::AgentStop { id }),
        // only a new agent is left, which the caller starts
        action @ Some(ChatAction::Command(_)) => return action,
        Some(ChatAction::Queue(index, action)) => {
            outbox.push(ClientMsg::AgentQueue { id, index, action })
        }
        // waiting messages come back into the box to change, ahead of anything already typed, and leave the queue
        Some(ChatAction::EditQueued(mut indices)) => {
            indices.retain(|&i| i < local.queue.len());
            let texts: Vec<String> = indices
                .iter()
                .map(|&i| local.queue[i].text.clone())
                .collect();
            for &i in &indices {
                // they were sent as the server sees them, and the box holds this machine's paths
                for path in &local.queue[i].attachments {
                    let path = mux_core::paths::local(path);
                    if !local.attachments.contains(&path) {
                        local.attachments.push(path);
                    }
                }
            }
            let mut parts = texts;
            if !local.draft.trim().is_empty() {
                parts.push(local.draft.trim_end().to_owned());
            }
            local.draft = parts.join("\n\n");
            indices.sort_unstable();
            for &i in indices.iter().rev() {
                local.queue.remove(i);
                outbox.push(ClientMsg::AgentQueue {
                    id,
                    index: i,
                    action: wire::QueueAction::Remove,
                });
            }
        }
        Some(ChatAction::Resume) => outbox.push(ClientMsg::AgentResume { id }),
        Some(ChatAction::StopBackground(task)) => {
            outbox.push(ClientMsg::AgentStopBackground { id, task })
        }
        Some(ChatAction::Answer(index, answers)) => {
            if let Some(Some(question)) = local.approvals.get(index) {
                outbox.push(ClientMsg::AgentAnswer {
                    id,
                    question: *question,
                    answers,
                });
            }
        }
        // the message goes back in the composer, after anything already typed there, and the chat winds back
        Some(ChatAction::Rewind(index)) => {
            let message = local.chat[..index]
                .iter()
                .filter(|item| matches!(item, ChatItem::User { .. }))
                .count();
            if let Some(ChatItem::User {
                text, attachments, ..
            }) = local.chat.get(index)
            {
                local.draft = if local.draft.trim().is_empty() {
                    text.clone()
                } else {
                    format!("{}\n\n{text}", local.draft.trim_end())
                };
                for path in attachments {
                    if !local.attachments.contains(path) {
                        local.attachments.push(path.clone());
                    }
                }
                outbox.push(ClientMsg::AgentRewind { id, message });
            }
        }
        // carrying out a plan leaves plan mode first, so the agent is free to make the changes
        Some(ChatAction::ImplementPlan) => {
            let mut settings = local.settings.clone();
            settings.plan = false;
            local.settings = settings.clone();
            outbox.push(ClientMsg::AgentConfigure { id, settings });
            outbox.push(ClientMsg::AgentSend {
                id,
                text: "Go ahead and carry out the plan.".to_owned(),
                attachments: Vec::new(),
            });
        }
        Some(ChatAction::Compact) => outbox.push(ClientMsg::AgentCompact { id }),
        Some(ChatAction::Decide(index, decision)) => {
            if let Some(Some(approval)) = local.approvals.get(index) {
                outbox.push(ClientMsg::AgentDecide {
                    id,
                    approval: *approval,
                    decision: match decision {
                        Decision::Allow => wire::Decision::Allow,
                        Decision::AlwaysAllow => wire::Decision::AlwaysAllow,
                        Decision::Deny => wire::Decision::Deny,
                    },
                });
            }
        }
        None => {}
    }
    None
}
