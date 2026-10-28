use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mux_core::protocol::{GroupInfo, SessionInfo};
use mux_core::{SessionKind, SessionStatus};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    next_id: u64,
    pub groups: Vec<GroupInfo>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        let mut store = match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Store>(&bytes) {
                Ok(store) => store,
                Err(err) => {
                    // keep the unreadable file around instead of overwriting someone's sessions
                    let backup = path.with_extension("json.bad");
                    eprintln!(
                        "mux: {} is unreadable ({err}), moved it to {}",
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
        for session in store.sessions_mut() {
            if matches!(
                session.status,
                SessionStatus::Running | SessionStatus::Waiting
            ) {
                session.status = SessionStatus::Idle;
            }
        }
        store
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // write then rename so a crash mid-write never leaves a half written file
        let tmp: PathBuf = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
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
        if let Some(group) = self.groups.iter_mut().find(|g| g.id == id) {
            group.path = path;
        }
    }

    // only empty groups go, so removing one can never take running sessions with it
    pub fn delete_group(&mut self, id: u64) {
        self.groups.retain(|g| g.id != id || !g.sessions.is_empty());
    }

    pub fn session_path(&self, id: u64) -> Option<&str> {
        self.groups
            .iter()
            .find(|g| g.sessions.iter().any(|s| s.id == id))?
            .path
            .as_deref()
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
        });
        id
    }

    pub fn delete_session(&mut self, id: u64) {
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
