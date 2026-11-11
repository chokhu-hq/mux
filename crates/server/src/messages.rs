use super::*;

impl Server {
    pub(super) fn message(&mut self, client: u64, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Shutdown => {
                self.quitting();
                let _ = self.stop.send(());
            }
            ClientMsg::CreateGroup { name, path } => {
                self.store.create_group(&name, path);
                self.changed();
            }
            ClientMsg::RenameGroup { id, name } => {
                self.store.rename_group(id, &name);
                self.changed();
            }
            ClientMsg::SetGroupPath { id, path } => {
                self.store.set_group_path(id, path);
                self.changed();
            }
            ClientMsg::DeleteGroup { id } => {
                self.store.delete_group(id);
                self.changed();
            }
            ClientMsg::MoveGroup { id, index } => {
                self.store.move_group(id, index);
                self.changed();
            }
            ClientMsg::ToggleGroup { id } => {
                self.store.toggle_group(id);
                self.changed();
            }
            ClientMsg::CreateSession { group, title, kind } => {
                let id = self.store.create_session(group, &title, kind);
                self.send(client, ServerMsg::Created { id });
                self.changed();
            }
            ClientMsg::DeleteSession { id } => {
                self.take_over(id);
                if let Some(path) = self.store.session(id).and_then(|s| s.worktree.clone()) {
                    worktree::remove(path);
                }
                self.processes.remove(&id);
                self.agents.remove(&id);
                let _ = std::fs::remove_file(self.agent_path(id));
                mcp::forget_claude(id);
                mcp::forget_images(id);
                self.attached.remove(&id);
                self.store.delete_session(id);
                self.changed();
            }
            ClientMsg::MoveSession { id, group, index } => {
                self.store.move_session(id, group, index);
                self.changed();
            }
            ClientMsg::Attach { id } => {
                self.attached.entry(id).or_default().insert(client);
                let idle = self
                    .store
                    .session(id)
                    .is_some_and(|s| s.status == SessionStatus::Idle);
                if !self.processes.contains_key(&id) && idle {
                    self.spawn(id);
                }
                if let Some(msg) = self.attached_msg(id) {
                    self.send(client, msg);
                }
            }
            ClientMsg::Detach { id } => {
                if let Some(watchers) = self.attached.get_mut(&id) {
                    watchers.remove(&client);
                }
            }
            ClientMsg::Restart { id } => {
                self.processes.remove(&id);
                self.spawn(id);
                if let Some(msg) = self.attached_msg(id) {
                    self.send_watchers(id, msg);
                }
            }
            ClientMsg::Input { id, data } => {
                if let Some(process) = self.processes.get_mut(&id) {
                    process.write(&data);
                }
            }
            ClientMsg::Resize { id, cols, rows } => {
                if let Some(process) = self.processes.get_mut(&id) {
                    process.resize(cols, rows);
                }
            }
            ClientMsg::AgentOpen { id } => {
                self.attached.entry(id).or_default().insert(client);
                let agent = self.agent(id);
                let pieces = transcript_pieces(id, agent);
                let (skills, provider) = (agent.skills(), agent.settings.provider);
                let (queue, held) = (agent.queue.clone(), agent.held);
                let tasks = agent.background();
                if let Some(tx) = self.clients.get(&client) {
                    tx.send_transcript(&pieces);
                }
                self.send(client, ServerMsg::AgentSkills { id, skills });
                self.send(client, ServerMsg::AgentQueue { id, queue, held });
                self.send(client, ServerMsg::AgentBackground { id, tasks });
                self.fetch_skills(id, provider);
                self.fetch_files(id);
                for provider in Provider::ALL {
                    if !self.models.contains_key(&provider) {
                        self.fetch_models(provider);
                    }
                }
            }
            ClientMsg::UseReset { provider } => {
                // only codex has resets to spend
                if provider == Provider::Codex {
                    let events = self.events.clone();
                    tokio::spawn(async move {
                        let result = models::use_codex_reset().await;
                        let _ = events.send(Event::ResetUsed(provider, result));
                    });
                }
            }
            ClientMsg::RefreshUsage => {
                for provider in Provider::ALL {
                    self.fetch_models(provider);
                }
                // a scan already running answers this request too
                if let Some(mut history) = self.history.take() {
                    let events = self.events.clone();
                    tokio::task::spawn_blocking(move || {
                        history.scan();
                        let _ = events.send(Event::History(history));
                    });
                }
            }
            ClientMsg::AgentConfigure { id, mut settings } => {
                let path = self.agent_path(id);
                let current = self.agent(id).settings.clone();
                let names = (self.model_name(&current), self.model_name(&settings));
                let agent = self.agent(id);
                let before = agent.settings.provider;
                // a conversation stays in the folder it started in, and moves to another agent only between turns
                if !agent.items.is_empty() {
                    settings.worktree = agent.settings.worktree;
                    if settings.provider != before
                        && !agent.switch(settings.clone(), names.0, names.1)
                    {
                        settings = agent.settings.clone();
                    }
                }
                agent.settings = settings.clone();
                if let Err(err) = agent.save(&path) {
                    log::error!("could not save {}: {err}", path.display());
                }
                let summary = agent.summary();
                // the other agent has its own skills, so the menu needs them instead
                if before != settings.provider {
                    agent.set_skills(Vec::new());
                }
                if let Some(info) = self.store.session_mut(id)
                    && info.summary.as_ref() != Some(&summary)
                {
                    info.summary = Some(summary);
                    self.changed();
                }
                if let Some(info) = self.store.session_mut(id)
                    && info.provider != settings.provider
                {
                    info.provider = settings.provider;
                    self.changed();
                }
                let switched = before != settings.provider;
                self.send_watchers(id, ServerMsg::AgentSettings { id, settings });
                if switched {
                    self.agent_changed(id);
                    let provider = self.agent(id).settings.provider;
                    self.fetch_skills(id, provider);
                }
            }
            ClientMsg::AgentSend {
                id,
                text,
                attachments,
            } => {
                self.agent(id).resumes = 0;
                self.take_over(id);
                self.agent_send(id, text, attachments);
            }
            ClientMsg::AgentDecide {
                id,
                approval,
                decision,
            } => {
                self.agent(id).decide(approval, decision);
                self.agent_changed(id);
            }
            ClientMsg::AgentStop { id } => {
                self.take_over(id);
                self.agent(id).stop();
                self.agent_changed(id);
            }
            ClientMsg::AgentCompact { id } => self.agent_compact(id),
            ClientMsg::AgentQueue { id, index, action } => {
                let send = self.agent(id).change_queue(index, action);
                self.agent_changed(id);
                if let Some(message) = send {
                    self.agent_send(id, message.text, message.attachments);
                }
            }
            ClientMsg::AgentStopBackground { id, task } => {
                self.agent(id).stop_background(&task);
            }
            ClientMsg::AgentResume { id } => {
                let send = self.agent(id).resume();
                self.agent_changed(id);
                if let Some(message) = send {
                    self.agent_send(id, message.text, message.attachments);
                }
            }
            ClientMsg::AgentRewind { id, message } => {
                let path = self.agent_path(id);
                let agent = self.agent(id);
                if agent.rewind(message) {
                    if let Err(err) = agent.save(&path) {
                        log::error!("could not save {}: {err}", path.display());
                    }
                    // the transcript got shorter, which item by item updates can't say, so it's sent whole
                    let pieces = transcript_pieces(id, agent);
                    let summary = agent.summary();
                    for watcher in self.attached.get(&id).into_iter().flatten() {
                        if let Some(tx) = self.clients.get(watcher) {
                            tx.send_transcript(&pieces);
                        }
                    }
                    if let Some(info) = self.store.session_mut(id) {
                        info.summary = Some(summary);
                    }
                    self.agent_changed(id);
                    self.changed();
                }
            }
            ClientMsg::AgentAnswer {
                id,
                question,
                answers,
            } => {
                self.agent(id).answer(question, answers);
                self.agent_changed(id);
            }
            ClientMsg::SetMcpServers { servers } => {
                let running: Vec<u64> = self
                    .agents
                    .iter()
                    .filter(|(_, agent)| agent.is_running())
                    .map(|(id, _)| *id)
                    .collect();
                mcp::forget_idle_claude(&running);
                self.store.mcp = servers;
                let names: Vec<&str> = self.store.mcp.iter().map(|s| s.name.as_str()).collect();
                self.mcp_status
                    .retain(|(name, ..)| names.contains(&name.as_str()));
                self.broadcast_mcp_status();
                self.changed();
                for tx in self.clients.values() {
                    let _ = tx.send(ServerMsg::McpServers {
                        servers: self.store.mcp.clone(),
                    });
                }
            }
            ClientMsg::RenameSession { id, title } => {
                // a name given by hand stays, since only an agent still called "New agent" is named automatically
                if let Some(info) = self.store.session_mut(id)
                    && !title.trim().is_empty()
                {
                    info.title = title.trim().to_owned();
                    self.changed();
                }
            }
        }
    }
}
