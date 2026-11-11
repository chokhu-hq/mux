use super::*;

#[cfg(test)]
pub(super) fn chat_items(
    transcript: &[AgentItem],
    busy: bool,
    provider: Provider,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    chat_items_from(transcript, &Settled::default(), busy, provider)
}

// the chat items for what comes after the settled part of the transcript
pub(super) fn chat_items_from(
    transcript: &[AgentItem],
    settled: &Settled,
    busy: bool,
    provider: Provider,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut items = Vec::new();
    let mut approvals = Vec::new();
    let legacy_retry = |item: &AgentItem| {
        matches!(item,
            AgentItem::Error { message } if message.starts_with("Mux will carry on by itself ")
        )
    };
    for (position, entry) in transcript.iter().enumerate().skip(settled.transcript) {
        let mut approval = None;
        // older servers saved one limit event as a reply followed by two errors
        if let AgentItem::Assistant { text } = entry
            && let Some(AgentItem::Error { message }) = transcript.get(position + 1)
            && text.trim() == message.trim()
            && transcript.get(position + 2).is_some_and(legacy_retry)
        {
            continue;
        }
        match entry {
            AgentItem::User {
                text,
                attachments,
                queued,
                took,
                ..
            } => items.push(ChatItem::User {
                took: *took,
                text: text.clone(),
                attachments: attachments
                    .iter()
                    .map(|path| mux_core::paths::local(path))
                    .collect(),
                queued: *queued,
            }),
            // a reply that has started but not streamed any words yet shows as thinking instead
            AgentItem::Assistant { text } if text.trim().is_empty() => continue,
            AgentItem::Assistant { text } => items.push(ChatItem::Assistant(text.clone())),
            AgentItem::Command {
                command,
                output,
                status,
                background,
                ..
            } => items.push(ChatItem::Tool(ToolCall {
                kind: ToolKind::Run,
                target: shell_body(command),
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
                background: *background,
                images: Vec::new(),
            })),
            AgentItem::FileChange { changes, .. } => {
                for change in changes {
                    items.push(ChatItem::Diff(FileDiff {
                        path: change.path.clone(),
                        lines: diff_lines(&change.diff),
                    }));
                    approvals.push(None);
                }
                continue;
            }
            AgentItem::Approval {
                id,
                title,
                detail,
                decision,
            } => {
                approval = Some(*id);
                items.push(ChatItem::Permission(PermissionRequest {
                    title: title.clone(),
                    detail: (!detail.is_empty()).then(|| shell_body(detail)),
                    decision: decision.map(|d| match d {
                        wire::Decision::Allow => Decision::Allow,
                        wire::Decision::AlwaysAllow => Decision::AlwaysAllow,
                        wire::Decision::Deny => Decision::Deny,
                    }),
                }));
            }
            AgentItem::Subagent {
                task,
                kind,
                background,
                status,
                activity,
                steps,
                result,
                tokens,
                ..
            } => items.push(ChatItem::Subagent(chat::Subagent {
                provider,
                task: task.clone(),
                kind: kind.clone(),
                status: tool_status(*status),
                background: *background,
                activity: activity.clone(),
                steps: steps
                    .iter()
                    .map(|step| ToolCall {
                        kind: tool_kind(&step.name),
                        target: match step.name.as_str() {
                            "Bash" => shell_body(&step.detail),
                            _ if step.detail.is_empty() => step.name.clone(),
                            _ => step.detail.clone(),
                        },
                        status: tool_status(step.status),
                        output: None,
                        background: false,
                        images: Vec::new(),
                    })
                    .collect(),
                result: result.clone(),
                tokens: *tokens,
            })),
            AgentItem::Tool {
                name,
                detail,
                output,
                status,
                images,
            } => items.push(ChatItem::Tool(ToolCall {
                kind: tool_kind(name),
                target: match (mcp_tool(name), detail.is_empty()) {
                    (Some(tool), true) => tool,
                    (Some(tool), false) => format!("{tool}  {detail}"),
                    (None, true) => name.clone(),
                    (None, false) => detail.clone(),
                },
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
                background: false,
                images: images.clone(),
            })),
            AgentItem::Compaction { status } => {
                items.push(ChatItem::Compaction(tool_status(*status)))
            }
            AgentItem::Switched { from, to } => items.push(ChatItem::Switched {
                from: from.clone(),
                to: to.clone(),
            }),
            AgentItem::Question {
                id,
                questions,
                answers,
                form,
            } => {
                approval = Some(*id);
                items.push(ChatItem::Question {
                    questions: questions.clone(),
                    answers: answers.clone(),
                    form: form.clone(),
                });
            }
            AgentItem::Plan { text } => {
                // only the latest plan, not yet replied to, can still be carried out
                let replied = transcript[position + 1..]
                    .iter()
                    .any(|i| matches!(i, AgentItem::User { .. } | AgentItem::Plan { .. }));
                items.push(ChatItem::Plan {
                    text: text.clone(),
                    actionable: !replied && !busy,
                });
            }
            AgentItem::Todos { steps } => items.push(ChatItem::Todos(steps.clone())),
            AgentItem::Reasoning {
                text,
                started,
                ended,
            } => items.push(ChatItem::Reasoning {
                text: text.clone(),
                started: *started,
                ended: *ended,
            }),
            AgentItem::Error { message } => {
                if let Some(AgentItem::Error { message: retry }) = transcript.get(position + 1)
                    && transcript.get(position + 1).is_some_and(legacy_retry)
                {
                    items.push(ChatItem::UsageLimit {
                        message: format!("{message}\n\n{retry}"),
                        retry_at: None,
                    });
                } else if legacy_retry(entry)
                    && position > 0
                    && matches!(transcript[position - 1], AgentItem::Error { .. })
                {
                    continue;
                } else {
                    items.push(ChatItem::Error(message.clone()))
                }
            }
            AgentItem::UsageLimit { message, retry_at } => {
                let replied = transcript[position + 1..]
                    .iter()
                    .any(|item| matches!(item, AgentItem::User { .. }));
                items.push(ChatItem::UsageLimit {
                    message: message.clone(),
                    retry_at: if replied { None } else { *retry_at },
                });
            }
            AgentItem::Retry {
                reason,
                attempt,
                of,
                status,
            } => items.push(ChatItem::Retry {
                reason: reason.clone(),
                attempt: *attempt,
                of: *of,
                status: match status {
                    ItemStatus::Running => ToolStatus::Running,
                    ItemStatus::Done => ToolStatus::Done,
                    ItemStatus::Failed | ItemStatus::Declined => ToolStatus::Failed,
                },
            }),
        }
        approvals.push(approval);
    }
    approvals.resize(items.len(), None);
    let (mut items, approvals) = summarise_changes(items, approvals, busy, settled.chat);
    chat::set_thinking(&mut items, busy && !settled.waiting);
    (items, approvals)
}

// a finished turn that touched several files ends with one line that opens them all for review
pub(super) fn summarise_changes(
    items: Vec<ChatItem>,
    approvals: Vec<Option<u64>>,
    busy: bool,
    // where these items start in the chat, since a summary points at its first edit card
    offset: usize,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut out = Vec::with_capacity(items.len());
    let mut out_approvals = Vec::with_capacity(items.len());
    // the first edit card's place in `out`, the files touched and the lines added and removed
    let mut turn: Option<(usize, Vec<String>, usize, usize)> = None;
    let close = |turn: &mut Option<(usize, Vec<String>, usize, usize)>,
                 out: &mut Vec<ChatItem>,
                 out_approvals: &mut Vec<Option<u64>>| {
        if let Some((first, files, added, removed)) = turn.take()
            && files.len() > 1
        {
            out.push(ChatItem::Changes(chat::Changes {
                files: files.len(),
                added,
                removed,
                first,
            }));
            out_approvals.push(None);
        }
    };
    for (item, approval) in items.into_iter().zip(approvals) {
        if matches!(item, ChatItem::User { .. }) {
            close(&mut turn, &mut out, &mut out_approvals);
        }
        if let ChatItem::Diff(diff) = &item {
            let count = |kind| diff.lines.iter().filter(|l| l.kind == kind).count();
            let (add, remove) = (count(chat::DiffKind::Add), count(chat::DiffKind::Remove));
            let entry = turn.get_or_insert_with(|| (offset + out.len(), Vec::new(), 0, 0));
            if !entry.1.contains(&diff.path) {
                entry.1.push(diff.path.clone());
            }
            entry.2 += add;
            entry.3 += remove;
        }
        out.push(item);
        out_approvals.push(approval);
    }
    // the turn still running isn't summed up until it's done
    if !busy {
        close(&mut turn, &mut out, &mut out_approvals);
    }
    (out, out_approvals)
}

// an mcp tool, named `mcp__server__tool` by both clis, read as `server › tool`
pub(super) fn mcp_tool(name: &str) -> Option<String> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    Some(format!("{server} › {tool}"))
}

pub(super) fn tool_kind(name: &str) -> ToolKind {
    if name.starts_with("mcp__") {
        return ToolKind::Mcp;
    }
    match name {
        "Read" | "NotebookRead" => ToolKind::Read,
        "Grep" | "Glob" | "WebSearch" | "WebFetch" | "LS" | "ToolSearch" => ToolKind::Search,
        "Edit" | "MultiEdit" | "Write" => ToolKind::Edit,
        _ => ToolKind::Run,
    }
}

pub(super) fn tool_status(status: ItemStatus) -> ToolStatus {
    match status {
        ItemStatus::Running => ToolStatus::Running,
        ItemStatus::Done => ToolStatus::Done,
        ItemStatus::Failed | ItemStatus::Declined => ToolStatus::Failed,
    }
}

// codex reports commands as `/usr/bin/bash -lc "..."`, and the quoted part is what people want to read
pub(super) fn shell_body(command: &str) -> String {
    let unwrapped = ["-lc ", "-c "].iter().find_map(|flag| {
        let (shell, body) = command.split_once(flag)?;
        let name = shell.trim().rsplit('/').next()?;
        ["bash", "sh", "zsh", "fish"]
            .contains(&name)
            .then_some(body.trim())
    });
    let Some(body) = unwrapped else {
        return command.to_owned();
    };
    if let Some(inner) = body.strip_prefix('"').and_then(|b| b.strip_suffix('"')) {
        inner.replace("\\\"", "\"").replace("\\\\", "\\")
    } else if let Some(inner) = body.strip_prefix('\'').and_then(|b| b.strip_suffix('\'')) {
        inner.to_owned()
    } else {
        body.to_owned()
    }
}

pub(super) fn diff_lines(diff: &str) -> Vec<DiffLine> {
    let headers = ["--- ", "+++ ", "@@", "diff ", "index "];
    let lines = diff
        .lines()
        .filter(|l| !headers.iter().any(|h| l.starts_with(h)));
    // a brand new file can arrive as plain contents rather than a patch
    let is_patch = diff.lines().any(|l| l.starts_with("@@"));
    lines
        .map(|line| {
            let (kind, text) = match line.chars().next() {
                _ if !is_patch => (DiffKind::Add, line),
                Some('+') => (DiffKind::Add, &line[1..]),
                Some('-') => (DiffKind::Remove, &line[1..]),
                Some(' ') => (DiffKind::Context, &line[1..]),
                _ => (DiffKind::Context, line),
            };
            DiffLine {
                kind,
                text: text.to_owned(),
            }
        })
        .collect()
}
