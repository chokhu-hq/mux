use std::sync::mpsc::Receiver;

use eframe::egui;
use mux_ui::{ChangedFile, DiffFiles, DiffScope};

// the changes open in the diff panel: which chat they're from, what's listed, and what's being read
pub(crate) struct Diffs {
    pub session: u64,
    // where git runs, the chat's worktree or else its project folder
    dir: Option<String>,
    // each file the chat changed, with the hunks the agent showed for it
    agent: Vec<(String, String)>,
    pub files: DiffFiles,
    // the repository's top folder and the commit the branch is compared with, once known
    repo: Option<Repo>,
    pub path: String,
    listing: Option<Receiver<Listing>>,
    reading: Option<Receiver<(String, String)>>,
}

#[derive(Clone)]
struct Repo {
    root: String,
    base: Option<(String, String)>,
}

struct Listing {
    scope: DiffScope,
    repo: Option<Repo>,
    files: Vec<ChangedFile>,
}

impl Diffs {
    pub fn open(
        ctx: &egui::Context,
        session: u64,
        dir: Option<String>,
        agent: Vec<(String, String)>,
        path: String,
    ) -> Self {
        let files = DiffFiles {
            files: agent_files(&agent, dir.as_deref()),
            loading: true,
            ..DiffFiles::default()
        };
        let mut diffs = Self {
            session,
            dir,
            files,
            agent,
            repo: None,
            path: String::new(),
            listing: None,
            reading: None,
        };
        diffs.list(ctx);
        diffs.read(ctx, path);
        diffs
    }

    pub fn set_scope(&mut self, ctx: &egui::Context, scope: DiffScope) {
        self.files.scope = scope;
        self.list(ctx);
        let path = self.path.clone();
        self.read(ctx, path);
    }

    pub fn select(&mut self, ctx: &egui::Context, index: usize) {
        if let Some(file) = self.files.files.get(index) {
            let path = file.path.clone();
            self.read(ctx, path);
        }
    }

    pub fn step(&mut self, ctx: &egui::Context, by: isize) {
        let count = self.files.files.len() as isize;
        if count == 0 {
            return;
        }
        let at = match self.files.current {
            Some(at) => (at as isize + by).rem_euclid(count),
            None => 0,
        };
        self.select(ctx, at as usize);
    }

    // which of the listed files is open and how many there are, for the prev and next buttons
    pub fn step_shown(&self) -> Option<(usize, usize)> {
        self.files.current.map(|at| (at, self.files.files.len()))
    }

    // the file as a message names it, relative to the repository or the project
    pub fn relative(&self, local: &str) -> String {
        let remote = mux_core::paths::remote(local);
        let roots = self
            .repo
            .iter()
            .map(|r| r.root.as_str())
            .chain(self.dir.as_deref());
        for root in roots {
            if let Some(rest) = remote.strip_prefix(&format!("{}/", root.trim_end_matches('/'))) {
                return rest.to_owned();
            }
        }
        remote
    }

    // the list or the diff that finished reading since last frame, true when the list changed
    pub fn poll(&mut self) -> (bool, Option<String>) {
        let mut listed = false;
        if let Some(listing) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.listing = None;
            if listing.scope == self.files.scope {
                self.repo = listing.repo;
                self.files.base = self
                    .repo
                    .as_ref()
                    .and_then(|r| r.base.as_ref())
                    .map(|(_, name)| name.clone());
                // a branch scope with nothing to compare against falls back to the uncommitted changes
                if self.files.scope == DiffScope::Branch && self.files.base.is_none() {
                    self.files.scope = DiffScope::Uncommitted;
                }
                self.files.files = listing.files;
                self.files.loading = false;
                self.files.current = self.index_of(&self.path);
                listed = true;
            }
        }
        let read = self.reading.as_ref().and_then(|rx| rx.try_recv().ok());
        let diff = read.and_then(|(path, unified)| {
            self.reading = None;
            (path == self.path).then_some(unified)
        });
        (listed, diff)
    }

    fn index_of(&self, path: &str) -> Option<usize> {
        self.files.files.iter().position(|f| f.path == path)
    }

    fn list(&mut self, ctx: &egui::Context) {
        let (dir, scope, agent) = (self.dir.clone(), self.files.scope, self.agent.clone());
        self.files.loading = true;
        if scope == DiffScope::Agent {
            self.files.files = agent_files(&self.agent, self.dir.as_deref());
            self.files.current = self.index_of(&self.path);
        }
        self.listing = Some(crate::in_background(ctx, move || {
            list(dir.as_deref(), scope, &agent)
        }));
    }

    fn read(&mut self, ctx: &egui::Context, path: String) {
        self.path = path.clone();
        self.files.current = self.index_of(&path);
        let fallback = self
            .agent
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, hunks)| hunks.clone())
            .unwrap_or_default();
        let scope = self.files.scope;
        let base = self
            .repo
            .as_ref()
            .and_then(|r| r.base.as_ref())
            .map(|(sha, _)| sha.clone());
        self.reading = Some(crate::in_background(ctx, move || {
            let unified = match (scope, base) {
                (DiffScope::Agent, _) => fallback,
                (DiffScope::Branch, Some(base)) => crate::file_diff(&path, &base, ""),
                _ => crate::file_diff(&path, "HEAD", &fallback),
            };
            (path, unified)
        }));
    }
}

// the agent's files with the lines its hunks add and remove
fn agent_files(agent: &[(String, String)], dir: Option<&str>) -> Vec<ChangedFile> {
    agent
        .iter()
        .map(|(path, hunks)| {
            let count = |sign: char| {
                hunks
                    .lines()
                    .filter(|l| l.starts_with(sign) && !l.starts_with("@@"))
                    .count()
            };
            ChangedFile {
                path: path.clone(),
                name: dir
                    .and_then(|d| path.strip_prefix(&format!("{}/", d.trim_end_matches('/'))))
                    .map_or_else(|| mux_core::paths::file_name(path), str::to_owned),
                added: count('+'),
                removed: count('-'),
            }
        })
        .collect()
}

fn git(dir: &str, args: &[&str]) -> Option<String> {
    mux_core::git(mux_core::paths::command("git"), dir, args)
        .ok()
        .map(|out| String::from_utf8_lossy(&out).into_owned())
}

// the files changed in a scope, read from git in the repository holding `dir`
fn list(dir: Option<&str>, scope: DiffScope, agent: &[(String, String)]) -> Listing {
    let root = dir.and_then(|dir| git(dir, &["rev-parse", "--show-toplevel"]));
    let Some(root) = root.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty()) else {
        return Listing {
            scope,
            repo: None,
            files: agent_files(agent, dir),
        };
    };
    let repo = Repo {
        base: base(&root),
        root: root.clone(),
    };
    let against = match scope {
        DiffScope::Agent => None,
        DiffScope::Branch => repo.base.as_ref().map(|(sha, _)| sha.clone()),
        DiffScope::Uncommitted => Some("HEAD".to_owned()),
    };
    let files = match against {
        None => agent_files(agent, dir),
        Some(against) => {
            let numstat = git(
                &root,
                &["diff", "--numstat", "-z", "--no-renames", &against, "--"],
            )
            .unwrap_or_default();
            let mut files = parse_numstat(&numstat);
            let untracked = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])
                .unwrap_or_default();
            for name in untracked.split('\0').filter(|n| !n.is_empty()) {
                let path = mux_core::paths::join(&root, name);
                let added = line_count(&mux_core::paths::local(&path));
                files.push((name.to_owned(), added, 0));
            }
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
                .into_iter()
                .map(|(name, added, removed)| ChangedFile {
                    path: mux_core::paths::join(&root, &name),
                    name,
                    added,
                    removed,
                })
                .collect()
        }
    };
    Listing {
        scope,
        repo: Some(repo),
        files,
    }
}

// where this branch left the default branch, trying origin's default and then the usual names
fn base(root: &str) -> Option<(String, String)> {
    for name in ["origin/HEAD", "main", "master"] {
        let Some(sha) = git(root, &["merge-base", "HEAD", name]) else {
            continue;
        };
        let shown = match name {
            "origin/HEAD" => git(root, &["rev-parse", "--abbrev-ref", name])
                .map(|n| n.trim().to_owned())
                .unwrap_or_else(|| name.to_owned()),
            _ => name.to_owned(),
        };
        return Some((sha.trim().to_owned(), shown));
    }
    None
}

// a new file counts every line as added, unless it's too big or isn't text
fn line_count(path: &str) -> usize {
    let small = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= 2 * 1024 * 1024);
    match std::fs::read(path) {
        Ok(bytes) if small && !bytes.iter().take(8192).any(|&b| b == 0) => {
            let lines = bytes.iter().filter(|&&b| b == b'\n').count();
            lines + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
        }
        _ => 0,
    }
}

// `git diff --numstat -z` without renames: added, removed and path, with `-` for binary files
fn parse_numstat(out: &str) -> Vec<(String, usize, usize)> {
    out.split('\0')
        .filter_map(|entry| {
            let mut parts = entry.trim_start_matches('\n').splitn(3, '\t');
            let added = parts.next()?;
            let removed = parts.next()?;
            let path = parts.next().filter(|p| !p.is_empty())?;
            let number = |n: &str| n.parse().unwrap_or(0);
            Some((path.to_owned(), number(added), number(removed)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_numstat;

    #[test]
    fn reads_numstat_with_binaries_and_odd_names() {
        let out = "3\t1\tsrc/main.rs\0-\t-\tassets/logo.png\x000\t12\tdocs/a file\twith tab.md\0";
        assert_eq!(
            parse_numstat(out),
            [
                ("src/main.rs".to_owned(), 3, 1),
                ("assets/logo.png".to_owned(), 0, 0),
                ("docs/a file\twith tab.md".to_owned(), 0, 12),
            ]
        );
        assert!(parse_numstat("").is_empty());
    }
}
