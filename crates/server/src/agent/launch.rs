use super::*;

impl Agent {
    pub(super) fn start(
        &mut self,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) -> io::Result<()> {
        let claude = self.settings.provider == Provider::Claude;
        let opencode = self.settings.provider == Provider::OpenCode;
        let mut command = if opencode {
            let mut command = Command::from(mux_core::command("opencode"));
            command.arg("acp");
            command
        } else if claude {
            let mut command = Command::from(mux_core::command("claude"));
            forget_parent_claude(&mut command);
            command.args(claude_args(
                &self.settings,
                self.session_id.as_deref(),
                self.resume_at.take().as_deref(),
            ));
            if let Some(path) = crate::mcp::claude_file(&self.mcp, session)? {
                command.arg("--mcp-config").arg(path);
            }
            command
        } else {
            let mut command = Command::from(mux_core::command("codex"));
            command.arg("app-server");
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(&cwd);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let group = match group::watch(&child) {
            Ok(group) => Some(group),
            Err(err) => {
                log::warn!("session {session}: couldn't watch the agent's processes: {err}");
                None
            }
        };
        let provider = self.settings.provider.label();
        log::info!("session {session}: started {provider} in {}", cwd.display());
        // what the cli says on stderr is often the only clue when it fails, so the start of it is kept in the log
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut kept = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    if kept < STDERR_LINES {
                        log::info!(target: "mux::agent", "session {session}: {provider}: {line}");
                    } else if kept == STDERR_LINES {
                        log::info!(target: "mux::agent", "session {session}: {provider}: more output left out");
                    }
                    kept += 1;
                }
            });
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;

        let (tx, mut outgoing) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = outgoing.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        self.events = Some(events.clone());
        let events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let event = Event::Agent {
                    session,
                    generation,
                    msg,
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::AgentExited {
                session,
                generation,
            });
        });

        self.generation = generation;
        self.session = session;
        self.started_mcp = self.mcp.clone();
        self.mcp_failed.clear();
        self.process = Some(Process {
            stdin: tx,
            child: Some(child),
            claude,
            group,
        });
        self.cwd = Some(cwd);
        if claude {
            self.launched = Some(self.settings.clone());
            self.claude_sent.clear();
            // without this an interrupt stops claude's background agents along with the turn
            self.write(json!({
                "type": "control_request",
                "request_id": "mux-initialize",
                "request": { "subtype": "initialize", "perTaskStopAffordance": true }
            }));
            return Ok(());
        }
        if opencode {
            self.opencode_initialize();
            return Ok(());
        }
        self.call(
            Call::Initialize,
            "initialize",
            // plan mode is still one of codex's experimental fields
            json!({
                "clientInfo": { "name": "mux", "title": "Mux", "version": mux_core::VERSION },
                "capabilities": { "experimentalApi": true },
            }),
        );
        Ok(())
    }
}
