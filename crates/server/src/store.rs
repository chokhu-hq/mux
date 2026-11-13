use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mux_core::protocol::{GroupInfo, McpServer, SessionInfo};
use mux_core::{SessionKind, SessionStatus};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    next_id: u64,
    pub groups: Vec<GroupInfo>,
    // moving a chat must not change the folder its conversation belongs to
    #[serde(default)]
    agent_folders: std::collections::HashMap<u64, String>,
    // agents that were mid-turn when the server last stopped, which it was too abrupt to stop itself
    #[serde(skip)]
    pub cut_off: Vec<u64>,
    // agents a usage limit stopped, and when to carry on with each, in unix seconds
    #[serde(default)]
    pub wake: std::collections::HashMap<u64, u64>,
    #[serde(default)]
    pub mcp: Vec<McpServer>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        let mut store = match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Store>(&bytes) {
                Ok(store) => store,
                Err(err) => {
                    // keep the unreadable file around instead of overwriting someone's sessions
                    // stamped, so a second bad file doesn't replace the first
                    let backup = path.with_extension(format!("json.bad-{}", now()));
                    log::error!(
                        "{} is unreadable ({err}), moved it to {}",
                        path.display(),
                        backup.display()
                    );
                    let _ = fs::rename(path, backup);
                    Store::default()
                }
            },
            Err(_) => Store::default(),
        };
        // processes never outlive the server, so anything that was live starts fresh
        let mut cut_off = Vec::new();
        for session in store.sessions_mut() {
            // one waiting on an answer can't pick up again, since the question went with the process
            if session.status == SessionStatus::Running && session.kind == SessionKind::Agent {
                cut_off.push(session.id);
            }
            if matches!(
                session.status,
                SessionStatus::Running | SessionStatus::Waiting
            ) {
                session.status = SessionStatus::Idle;
            }
        }
        store.cut_off = cut_off;
        // ids are never reused, even when the state was lost, since an old chat saved under one would come back
        let saved = fs::read_dir(path.with_file_name("agents"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".json")?.parse::<u64>().ok()
            });
        let used = store
            .groups
            .iter()
            .flat_map(|g| std::iter::once(g.id).chain(g.sessions.iter().map(|s| s.id)))
            .chain(saved)
            .max()
            .unwrap_or(0);
        store.next_id = store.next_id.max(used);
        store
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // write then rename so a crash mid-write never leaves a half written file
        let tmp: PathBuf = path.with_extension("json.tmp");
        // private, because it holds the mcp servers' keys and tokens
        mux_core::write_private(
            &tmp,
            &serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(tmp, path)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn sessions_mut(&mut self) -> impl Iterator<Item = &mut SessionInfo> {
        self.groups.iter_mut().flat_map(|g| &mut g.sessions)
    }

    pub fn session(&self, id: u64) -> Option<&SessionInfo> {
        self.groups
            .iter()
            .flat_map(|g| &g.sessions)
            .find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: u64) -> Option<&mut SessionInfo> {
        self.sessions_mut().find(|s| s.id == id)
    }

    pub fn create_group(&mut self, name: &str, path: Option<String>) -> u64 {
        let id = self.next_id();
        self.groups.push(GroupInfo {
            id,
            name: name.to_owned(),
            path,
            branch: None,
            open: true,
            sessions: Vec::new(),
        });
        id
    }

    pub fn rename_group(&mut self, id: u64, name: &str) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id)
            && !name.trim().is_empty()
        {
            group.name = name.trim().to_owned();
        }
    }

    pub fn set_group_path(&mut self, id: u64, path: Option<String>) {
        let sessions: Vec<_> = self
            .groups
            .iter()
            .filter(|g| g.id == id)
            .flat_map(|g| g.sessions.iter().map(|s| s.id))
            .collect();
        for session in sessions {
            self.keep_agent_folder(session);
        }
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.path = path;
        }
    }

    // only empty groups go, so removing one can never take running sessions with it
    pub fn delete_group(&mut self, id: u64) {
        self.groups.retain(|g| g.id != id || !g.sessions.is_empty());
    }

    // a chat keeps its original folder when moved, with its worktree taking precedence
    pub fn session_path(&self, id: u64) -> Option<&str> {
        if let Some(worktree) = self.session(id).and_then(|s| s.worktree.as_deref()) {
            return Some(worktree);
        }
        self.agent_folders
            .get(&id)
            .map(String::as_str)
            .or_else(|| self.project_path(id))
    }

    fn keep_agent_folder(&mut self, id: u64) {
        if self.session(id).is_some_and(|s| {
            s.kind == SessionKind::Agent
                && s.summary
                    .as_ref()
                    .is_some_and(|summary| summary.messages > 0)
        }) && let Some(path) = self.project_path(id).map(str::to_owned)
        {
            self.agent_folders.entry(id).or_insert(path);
        }
    }

    pub fn project_path(&self, id: u64) -> Option<&str> {
        self.groups
            .iter()
            .find(|g| g.sessions.iter().any(|s| s.id == id))?
            .path
            .as_deref()
    }

    pub fn move_group(&mut self, id: u64, index: usize) {
        let Some(at) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        let group = self.groups.remove(at);
        let index = if at < index { index - 1 } else { index };
        self.groups.insert(index.min(self.groups.len()), group);
    }

    pub fn toggle_group(&mut self, id: u64) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.open = !group.open;
        }
    }

    pub fn create_session(&mut self, group: Option<u64>, title: &str, kind: SessionKind) -> u64 {
        let index = match group.and_then(|id| self.groups.iter().position(|g| g.id == id)) {
            Some(index) => index,
            None if self.groups.is_empty() => {
                self.create_group("Sessions", None);
                0
            }
            None => 0,
        };
        let id = self.next_id();
        let folder = self.groups[index]
            .path
            .as_deref()
            .map(mux_core::display_path);
        let meta = match kind {
            SessionKind::Shell => folder.unwrap_or_else(|| "~".to_owned()),
            SessionKind::Task => title.to_owned(),
            SessionKind::Agent => "not started".to_owned(),
        };
        let group = &mut self.groups[index];
        group.open = true;
        group.sessions.push(SessionInfo {
            id,
            title: title.to_owned(),
            kind,
            status: SessionStatus::Idle,
            meta,
            updated: now(),
            provider: Default::default(),
            worktree: None,
            branch: None,
            summary: None,
        });
        id
    }

    pub fn delete_session(&mut self, id: u64) {
        self.agent_folders.remove(&id);
        for group in &mut self.groups {
            group.sessions.retain(|s| s.id != id);
        }
    }

    // `index` counts positions before the session is taken out, the way a drop target sees the list
    pub fn move_session(&mut self, id: u64, group: u64, index: usize) {
        let Some(from) = self
            .groups
            .iter()
            .position(|g| g.sessions.iter().any(|s| s.id == id))
        else {
            return;
        };
        let Some(to) = self.groups.iter().position(|g| g.id == group) else {
            return;
        };
        let at = self.groups[from]
            .sessions
            .iter()
            .position(|s| s.id == id)
            .unwrap_or(0);
        self.keep_agent_folder(id);
        let session = self.groups[from].sessions.remove(at);
        let mut index = index;
        if from == to && at < index {
            index -= 1;
        }
        let target = &mut self.groups[to];
        target.open = true;
        target
            .sessions
            .insert(index.min(target.sessions.len()), session);
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(store: &Store) -> Vec<Vec<u64>> {
        store
            .groups
            .iter()
            .map(|g| g.sessions.iter().map(|s| s.id).collect())
            .collect()
    }

    #[test]
    fn moved_chats_keep_their_folder_across_state_round_trips() {
        let mut store = Store::default();
        let a = store.create_group("a", Some("/project-a".into()));
        let b = store.create_group("b", Some("/project-b".into()));
        let chat = store.create_session(Some(a), "chat", SessionKind::Agent);
        store.session_mut(chat).unwrap().summary = Some(mux_core::protocol::AgentSummary {
            messages: 1,
            ..Default::default()
        });
        store.move_session(chat, b, 0);
        store.set_group_path(b, Some("/project-c".into()));
        let mut store: Store =
            serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        assert_eq!(store.session_path(chat), Some("/project-a"));
        assert_eq!(store.project_path(chat), Some("/project-c"));
        store.session_mut(chat).unwrap().worktree = Some("/checkout".into());
        assert_eq!(store.session_path(chat), Some("/checkout"));
        store.delete_session(chat);
        assert!(!store.agent_folders.contains_key(&chat));
    }

    #[test]
    fn changing_a_project_folder_preserves_existing_chats() {
        let mut store = Store::default();
        let group = store.create_group("a", None);
        let chat = store.create_session(Some(group), "chat", SessionKind::Agent);
        store.set_group_path(group, Some("/first".into()));
        assert_eq!(store.session_path(chat), Some("/first"));
        store.session_mut(chat).unwrap().summary = Some(mux_core::protocol::AgentSummary {
            messages: 1,
            ..Default::default()
        });
        store.set_group_path(group, Some("/second".into()));
        assert_eq!(store.session_path(chat), Some("/first"));
        let new_chat = store.create_session(Some(group), "new", SessionKind::Agent);
        assert_eq!(store.session_path(new_chat), Some("/second"));
    }

    #[test]
    fn moves_within_and_across_groups() {
        let mut store = Store::default();
        let a = store.create_group("a", None);
        let b = store.create_group("b", None);
        let one = store.create_session(Some(a), "one", SessionKind::Shell);
        let two = store.create_session(Some(a), "two", SessionKind::Shell);
        let three = store.create_session(Some(a), "three", SessionKind::Shell);

        store.move_session(one, a, 3);
        assert_eq!(order(&store), vec![vec![two, three, one], vec![]]);

        store.move_session(three, b, 0);
        assert_eq!(order(&store), vec![vec![two, one], vec![three]]);
    }

    #[test]
    fn groups_move_up_and_down() {
        let mut store = Store::default();
        let [a, b, c] = ["a", "b", "c"].map(|name| store.create_group(name, None));
        let ids = |store: &Store| store.groups.iter().map(|g| g.id).collect::<Vec<_>>();
        store.move_group(a, 3);
        assert_eq!(ids(&store), [b, c, a]);
        store.move_group(a, 0);
        assert_eq!(ids(&store), [a, b, c]);
        store.move_group(c, 1);
        assert_eq!(ids(&store), [a, c, b]);
    }

    #[test]
    fn only_empty_groups_are_removed() {
        let mut store = Store::default();
        let full = store.create_group("full", None);
        let empty = store.create_group("empty", Some("/tmp".to_owned()));
        store.create_session(Some(full), "shell", SessionKind::Shell);

        store.delete_group(full);
        store.delete_group(empty);
        let names: Vec<&str> = store.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["full"]);
    }

    #[test]
    fn a_lost_state_never_hands_out_an_id_an_old_chat_was_saved_under() {
        let dir = std::env::temp_dir().join(format!("mux-store-ids-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join("agents").join("7.json"), "{}").unwrap();
        std::fs::write(dir.join("state.json"), "not json").unwrap();
        let mut store = Store::load(&dir.join("state.json"));
        assert!(store.create_session(None, "x", SessionKind::Agent) > 7);
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("state.json.bad-")
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loading_resets_live_sessions() {
        let dir = std::env::temp_dir().join(format!("mux-store-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut store = Store::default();
        let id = store.create_session(None, "shell", SessionKind::Shell);
        store.session_mut(id).unwrap().status = SessionStatus::Running;
        store.save(&path).unwrap();

        let loaded = Store::load(&path);
        assert_eq!(loaded.session(id).unwrap().status, SessionStatus::Idle);
        let _ = fs::remove_dir_all(dir);
    }
}
