use super::*;

impl Server {
    pub(super) fn agent_path(&self, id: u64) -> PathBuf {
        self.state
            .with_file_name("agents")
            .join(format!("{id}.json"))
    }

    // the moment a limit resets is when it's lifted, so the agent waits a little longer to be sure
    pub(super) fn wake_at(&self, id: u64, reset: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let wait = std::time::Duration::from_secs(reset.saturating_sub(now) + WAKE_SLACK);
        let events = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            let _ = events.send(Event::Wake(id, reset));
        });
    }

    pub(super) fn wake(&mut self, id: u64, reset: u64) {
        // the user took over since, or a later limit set another time
        if self.store.wake.get(&id) != Some(&reset) {
            return;
        }
        self.store.wake.remove(&id);
        self.changed();
        let agent = self.agent(id);
        if agent.resumes >= MAX_RESUMES {
            agent.error(format!(
                "The usage limit reset, but this agent has picked back up {MAX_RESUMES} times in a row, so it's waiting for you this time."
            ));
            self.agent_changed(id);
            return;
        }
        agent.resumes += 1;
        log::info!("session {id}: carrying on now that the usage limit reset");
        self.agent_send(id, LIMIT_RESET.to_owned(), Vec::new());
    }

    // the user is acting on the agent themselves, so it no longer carries on by itself
    pub(super) fn take_over(&mut self, id: u64) {
        if self.store.wake.remove(&id).is_some() {
            self.changed();
        }
    }

    // agents cut off mid-turn by a crash carry on, a few times at most so a turn that crashes the server can't loop
    pub(super) fn resume_cut_off(&mut self) {
        for id in std::mem::take(&mut self.store.cut_off) {
            // one waiting for a usage limit to reset carries on then instead
            if self.store.wake.contains_key(&id) {
                continue;
            }
            let agent = self.agent(id);
            if agent.resumes >= MAX_RESUMES {
                agent.error(format!(
                    "Mux restarted while this agent was working. It has picked back up {MAX_RESUMES} times in a row, so it's waiting for you this time."
                ));
                self.agent_changed(id);
                continue;
            }
            agent.resumes += 1;
            log::info!("session {id}: carrying on a turn the last server didn't finish");
            self.agent_send(id, CUT_OFF.to_owned(), Vec::new());
        }
    }

    pub(super) fn agent(&mut self, id: u64) -> &mut Agent {
        let path = self.agent_path(id);
        self.agents.entry(id).or_insert_with(|| Agent::load(&path))
    }

    pub(super) fn agent_send(&mut self, id: u64, text: String, attachments: Vec<String>) {
        let Some(info) = self.store.session_mut(id) else {
            return;
        };
        // a fresh agent takes its name from the first thing it's asked
        if info.title == "New agent" {
            let first = text.lines().next().unwrap_or_default().trim();
            let mut title: String = first.chars().take(48).collect();
            if first.chars().count() > 48 {
                title.push('…');
            }
            if !title.is_empty() {
                info.title = title;
            }
        }
        // the first message of a worktree agent makes its checkout, named after what it was asked
        let wants_worktree =
            info.worktree.is_none() && self.agents.get(&id).is_some_and(|a| a.settings.worktree);
        if wants_worktree {
            let title = self
                .store
                .session(id)
                .map(|s| s.title.clone())
                .unwrap_or_default();
            match self
                .store
                .session_path(id)
                .map(|repo| worktree::add(repo, &title, id))
            {
                Some(Ok(path)) => {
                    if let Some(info) = self.store.session_mut(id) {
                        info.worktree = Some(path);
                    }
                }
                Some(Err(reason)) => {
                    self.agent(id).error(reason);
                    self.agent_changed(id);
                    self.changed();
                    return;
                }
                None => {}
            }
        }
        // plan mode needs codex's model named, so the default stands in when none was picked
        let default_model = self
            .models
            .get(&Provider::Codex)
            .and_then(|list| list.iter().find(|m| m.is_default))
            .map(|m| m.id.clone());
        self.agent(id).default_model = default_model;
        let mcp = self.store.mcp.clone();
        self.agent(id).mcp = mcp;
        let folder = self.store.session_path(id).map(PathBuf::from);
        self.generation += 1;
        let (generation, events) = (self.generation, self.events.clone());
        let agent = self.agent(id);
        // an agent's sandbox is its folder, so falling back to the home folder would expose everything
        match folder {
            None => agent.error(
                "This group has no project folder yet. Choose one before starting an agent."
                    .to_owned(),
            ),
            Some(dir) if !dir.is_dir() => {
                agent.error(format!(
                    "The project folder {} doesn't exist any more.",
                    mux_core::display_path(&dir.to_string_lossy())
                ));
            }
            Some(dir) => agent.send(text, attachments, id, generation, dir, &events),
        }
        self.agent_changed(id);
        self.changed();
    }

    // skills are read fresh each time a chat opens, so a new or edited one shows up without restarting
    pub(super) fn fetch_skills(&self, id: u64, provider: Provider) {
        let Some(folder) = self.store.session_path(id).map(PathBuf::from) else {
            return;
        };
        let events = self.events.clone();
        tokio::spawn(async move {
            let skills = match provider {
                Provider::Claude => tokio::task::spawn_blocking(move || skills::claude(&folder))
                    .await
                    .unwrap_or_default(),
                Provider::Codex => skills::codex(&folder).await.unwrap_or_else(|err| {
                    log::warn!("could not list codex skills: {err}");
                    Vec::new()
                }),
                // opencode lists its commands itself once its session opens
                Provider::OpenCode => return,
            };
            let _ = events.send(Event::Skills(id, provider, skills));
        });
    }

    // what a chat's switch line calls a model, the way the picker shows it
    pub(super) fn model_name(&self, settings: &mux_core::protocol::AgentSettings) -> String {
        let list = self.models.get(&settings.provider);
        let model = list.and_then(|list| {
            settings
                .model
                .as_deref()
                .and_then(|id| list.iter().find(|m| m.id == id))
                .or_else(|| list.iter().find(|m| m.is_default))
        });
        match model {
            Some(model) => model.name.clone(),
            None => settings
                .model
                .clone()
                .unwrap_or_else(|| settings.provider.label().to_owned()),
        }
    }

    // listed fresh each time a chat opens, so new files can be mentioned without restarting
    pub(super) fn fetch_files(&self, id: u64, refresh: bool) {
        let Some(folder) = self.store.session_path(id).map(PathBuf::from) else {
            return;
        };
        let events = self.events.clone();
        tokio::task::spawn_blocking(move || {
            let _ = events.send(Event::Files(id, files::list(&folder), refresh));
        });
    }

    // the model list comes with the plan limits, so fetching it again is how usage is refreshed
    pub(super) fn fetch_models(&mut self, provider: Provider) {
        if !self.fetching_models.insert(provider) {
            return;
        }
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = models::fetch(provider).await.map_err(|e| e.to_string());
            let _ = events.send(Event::Models(provider, result));
        });
    }

    pub(super) fn agent_compact(&mut self, id: u64) {
        let Some(dir) = self
            .store
            .session_path(id)
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
        else {
            return;
        };
        self.generation += 1;
        let (generation, events) = (self.generation, self.events.clone());
        self.agent(id).compact(id, generation, dir, &events);
        self.agent_changed(id);
        self.changed();
    }

    // pushes what changed to the clients watching, and keeps the sidebar status in step
    pub(super) fn agent_changed(&mut self, id: u64) {
        // the agents are about to stop with the server, which shouldn't be saved as them still working
        if self.leaving {
            return;
        }
        // saved before the app hears of it, so a crash straight after still carries on at the reset
        if let Some(reset) = self.agents.get_mut(&id).and_then(Agent::take_pause) {
            self.store.wake.insert(id, reset);
            self.changed();
            self.wake_at(id, reset);
        }
        let path = self.agent_path(id);
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let agent::Changes {
            items,
            appends,
            busy,
        } = agent.take_changes();
        let status = agent.status();
        let provider = agent.settings.provider;
        let label = provider.label();
        let usage = agent.usage.take();
        let context = agent.take_context();
        let skills = agent.take_skills();
        let queue = agent.take_queue();
        let background = agent.take_background();
        let mcp_states = agent.take_mcp_states();
        // saved when a turn starts or ends rather than on every streamed word, and the sidebar's summary with it
        let settled = busy.is_some()
            || agent.ids_changed()
            || (!agent.busy && (!items.is_empty() || !appends.is_empty()));
        if settled && let Err(err) = agent.save(&path) {
            log::error!("could not save {}: {err}", path.display());
        }
        let summary = settled.then(|| agent.summary());
        for (index, item) in items {
            // whatever the agent showed as an error is worth having when someone sends their log
            if let AgentItem::Error { message } = &item {
                log::warn!("session {id}: {label} error: {message}");
            }
            let item = fit_to_send(item);
            self.send_watchers(id, ServerMsg::AgentItem { id, index, item });
        }
        for (index, at, text) in appends {
            self.send_watchers(
                id,
                ServerMsg::AgentAppend {
                    id,
                    index,
                    at,
                    text,
                },
            );
        }
        if let Some(busy) = busy {
            self.send_watchers(id, ServerMsg::AgentBusy { id, busy });
            // the turn may have added or removed files, so `@` mentions see them without reopening the chat
            if !busy {
                self.fetch_files(id, true);
            }
        }
        if let Some(windows) = usage {
            self.set_usage(provider, windows);
        }
        if let Some(context) = context {
            self.send_watchers(id, ServerMsg::AgentContext { id, context });
        }
        if let Some(skills) = skills {
            self.send_watchers(id, ServerMsg::AgentSkills { id, skills });
        }
        if let Some(tasks) = background {
            self.send_watchers(id, ServerMsg::AgentBackground { id, tasks });
        }
        if !mcp_states.is_empty() {
            for (name, state) in mcp_states {
                match self
                    .mcp_status
                    .iter_mut()
                    .find(|(n, p, _)| *n == name && *p == provider)
                {
                    Some(entry) => entry.2 = state,
                    None => self.mcp_status.push((name, provider, state)),
                }
            }
            self.broadcast_mcp_status();
        }
        if let Some((queue, held)) = queue {
            // waiting messages are kept even when nothing else about the chat changed
            if let Some(agent) = self.agents.get(&id)
                && let Err(err) = agent.save(&path)
            {
                log::error!("could not save {}: {err}", path.display());
            }
            self.send_watchers(id, ServerMsg::AgentQueue { id, queue, held });
        }
        if let Some(info) = self.store.session_mut(id)
            && info.status != status
        {
            info.status = status;
            info.meta = label.to_owned();
            self.changed();
        }
        if let (Some(summary), Some(info)) = (summary, self.store.session_mut(id))
            && info.summary.as_ref() != Some(&summary)
        {
            info.summary = Some(summary);
            self.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_worktree_creation_is_retried_without_starting_an_agent() {
        let root = std::env::temp_dir().join(format!("mux-retry-worktree-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (events, _) = mpsc::unbounded_channel();
        let (stop, _) = mpsc::unbounded_channel();
        let mut server = Server {
            mcp_status: Vec::new(),
            store: Store::default(),
            state: root.join("state.json"),
            clients: HashMap::new(),
            processes: HashMap::new(),
            agents: HashMap::new(),
            seen: HashMap::new(),
            models: HashMap::new(),
            fetching_models: HashSet::new(),
            files_sent: HashMap::new(),
            usage: HashMap::new(),
            resets: HashMap::new(),
            history: None,
            attached: HashMap::new(),
            generation: 0,
            events,
            stop,
            leaving: false,
        };
        let group = server
            .store
            .create_group("not a repo", Some(root.to_string_lossy().into_owned()));
        let id =
            server
                .store
                .create_session(Some(group), "New agent", mux_core::SessionKind::Agent);
        server.agent(id).settings.worktree = true;
        for _ in 0..2 {
            server.agent_send(id, "hello".into(), Vec::new());
            assert!(server.agent(id).items.iter().all(|item| matches!(item,
                AgentItem::Error { message } if message.contains("isn't a git repository"))));
        }
        assert_eq!(server.agent(id).items.len(), 2);
        assert!(server.store.session(id).unwrap().worktree.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
