use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mux_core::protocol::{DirEntry, FileKind};

// enough for big repos without sending the client a huge list
const LIMIT: usize = 20_000;

// the project's files for `@` mentions, relative and with forward slashes, honouring .gitignore where there is one
pub fn list(dir: &Path) -> Vec<String> {
    let output = mux_core::git(
        mux_core::command("git"),
        dir,
        [
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    );
    if let Ok(stdout) = output {
        let mut files: Vec<String> = stdout
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        files.sort();
        files.dedup();
        files.truncate(LIMIT);
        return files;
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    files
}

// outside git, hidden folders and the usual build and dependency folders are skipped
fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= LIMIT {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if !name.starts_with('.') && !matches!(name.as_ref(), "node_modules" | "target") {
                walk(root, &path, files);
            }
        } else if kind.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

// one chunk of a file read for a preview, which keeps each frame well under the limit
pub const CHUNK: u64 = 1024 * 1024;
// a folder this full is cut short rather than sent whole
const DIR_LIMIT: usize = 5_000;
// how long a listing for search is reused, since each keystroke asks again
const SEARCH_TTL: Duration = Duration::from_secs(10);

// `path` inside `root`, refusing anything that leads out of it, through `..` or a symlink
pub fn resolve(root: &Path, path: &str) -> Result<(PathBuf, PathBuf), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("Couldn't open the project folder: {e}"))?;
    let target = root
        .join(path)
        .canonicalize()
        .map_err(|e| format!("Couldn't open {path}: {e}"))?;
    if !target.starts_with(&root) {
        return Err("That's outside the project folder.".to_owned());
    }
    Ok((root, target))
}

// a path below the root written the way git and the client write it
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

pub fn list_dir(root: &Path, dir: &str) -> Result<(Vec<DirEntry>, bool), String> {
    let (root, folder) = resolve(root, dir)?;
    let entries = std::fs::read_dir(&folder).map_err(|e| format!("Couldn't list {dir}: {e}"))?;
    let marks = Marks::read(&root);
    let base = relative(&root, &folder);
    let mut listed = Vec::new();
    let mut truncated = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        if listed.len() >= DIR_LIMIT {
            truncated = true;
            break;
        }
        // a symlink is shown as what it points at
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let path = if base.is_empty() {
            name.clone()
        } else {
            format!("{base}/{name}")
        };
        listed.push(DirEntry {
            git: marks.mark(&path, meta.is_dir()),
            ignored: marks.ignored(&path),
            dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            name,
        });
    }
    listed.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((listed, truncated))
}

// git's view of the files below a root, from one `git status`
#[derive(Default)]
struct Marks {
    // paths relative to the root with their status letter, where untracked folders end in a slash
    changed: Vec<(String, char)>,
    ignored: Vec<String>,
}

impl Marks {
    fn read(root: &Path) -> Self {
        let git = |args: &[&str]| mux_core::git(mux_core::command("git"), root, args);
        // status names paths from the top of the repository, and the root may be a folder inside it
        let Ok(prefix) = git(&["rev-parse", "--show-prefix"]) else {
            return Self::default();
        };
        let prefix = String::from_utf8_lossy(&prefix).trim().to_owned();
        match git(&["status", "--porcelain=v1", "-z", "--ignored", "--", "."]) {
            Ok(out) => Self::parse(&out, &prefix),
            Err(_) => Self::default(),
        }
    }

    fn parse(out: &[u8], prefix: &str) -> Self {
        let mut marks = Self::default();
        let mut fields = out.split(|b| *b == 0);
        while let Some(field) = fields.next() {
            if field.len() < 4 {
                continue;
            }
            let (x, y) = (field[0] as char, field[1] as char);
            let path = String::from_utf8_lossy(&field[3..]);
            // a rename or copy is followed by the path it came from
            if matches!(x, 'R' | 'C') {
                fields.next();
            }
            let Some(path) = path.strip_prefix(prefix) else {
                continue;
            };
            let letter = match (x, y) {
                ('!', '!') => {
                    marks.ignored.push(path.to_owned());
                    continue;
                }
                ('?', '?') => 'U',
                ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => '!',
                (x, ' ') => x,
                (_, y) => y,
            };
            marks.changed.push((path.to_owned(), letter));
        }
        marks
    }

    // git lists a whole untracked or ignored folder once, with a slash on the end
    fn under(listed: &str, path: &str) -> bool {
        listed == path || (listed.ends_with('/') && path.starts_with(listed))
    }

    fn ignored(&self, path: &str) -> bool {
        let dir = format!("{path}/");
        self.ignored
            .iter()
            .any(|i| Self::under(i, path) || *i == dir)
    }

    // a folder takes the letter its changed files share, or M when they differ
    fn mark(&self, path: &str, dir: bool) -> Option<char> {
        let inside = format!("{path}/");
        let mut found = None;
        for (listed, letter) in &self.changed {
            let hit = Self::under(listed, path)
                || *listed == inside
                || (dir && listed.starts_with(&inside));
            if hit {
                match found {
                    None => found = Some(*letter),
                    Some(seen) if seen != *letter => return Some('M'),
                    _ => {}
                }
            }
        }
        found
    }
}

pub struct Chunk {
    pub total: u64,
    pub bytes: Vec<u8>,
    pub kind: FileKind,
}

pub fn read_file(root: &Path, path: &str, offset: u64, len: u64) -> Result<Chunk, String> {
    use std::io::{Read, Seek, SeekFrom};
    let (_, file) = resolve(root, path)?;
    let meta = std::fs::metadata(&file).map_err(|e| format!("Couldn't open {path}: {e}"))?;
    if meta.is_dir() {
        return Err("This is a folder, not a file.".to_owned());
    }
    let mut handle =
        std::fs::File::open(&file).map_err(|e| format!("Couldn't open {path}: {e}"))?;
    let mut head = Vec::new();
    (&mut handle)
        .take(8192)
        .read_to_end(&mut head)
        .map_err(|e| format!("Couldn't read {path}: {e}"))?;
    let kind = kind(&head, path);
    let mut bytes = Vec::new();
    handle
        .seek(SeekFrom::Start(offset))
        .and_then(|_| handle.take(len.min(CHUNK)).read_to_end(&mut bytes))
        .map_err(|e| format!("Couldn't read {path}: {e}"))?;
    Ok(Chunk {
        total: meta.len(),
        bytes,
        kind,
    })
}

// what a file holds, from its first bytes, with svg counted as a picture
pub fn kind(head: &[u8], path: &str) -> FileKind {
    const IMAGES: [&[u8]; 6] = [
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
        b"GIF87a",
        b"GIF89a",
        b"BM",
        b"\0\0\x01\0",
    ];
    if head.starts_with(b"%PDF-") {
        return FileKind::Pdf;
    }
    let webp = head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP";
    if webp || IMAGES.iter().any(|magic| head.starts_with(magic)) {
        return FileKind::Image;
    }
    let text = !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            // the head can end partway through a character
            Err(e) => e.error_len().is_none(),
        };
    if !text {
        return FileKind::Binary;
    }
    let svg = path.to_ascii_lowercase().ends_with(".svg")
        || String::from_utf8_lossy(head)
            .trim_start()
            .starts_with("<svg");
    if svg { FileKind::Image } else { FileKind::Text }
}

// a root's files and when they were listed
type Listed = (PathBuf, Instant, Arc<Vec<String>>);

// the project's files that best match `query`, as a fuzzy match on their paths
pub fn search(root: &Path, query: &str, limit: usize) -> Vec<String> {
    static CACHE: Mutex<Option<Listed>> = Mutex::new(None);
    let files = {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        match cache.as_ref() {
            Some((dir, at, files)) if dir == root && at.elapsed() < SEARCH_TTL => files.clone(),
            _ => {
                let files = Arc::new(list(root));
                *cache = Some((root.to_owned(), Instant::now(), files.clone()));
                files
            }
        }
    };
    best(&files, query, limit)
}

fn best(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let mut scored: Vec<(i64, &String)> = files
        .iter()
        .filter_map(|f| Some((score(f, query)?, f)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, f)| f.clone())
        .collect()
}

// higher for runs of letters together and letters that start a word, and for hits in the file's own name
fn score(path: &str, query: &str) -> Option<i64> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return None;
    }
    let chars: Vec<char> = path.chars().collect();
    let mut score = 0i64;
    let mut at = 0;
    let mut last: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if at == query.len() {
            break;
        }
        if c.to_lowercase().eq(std::iter::once(query[at])) {
            score += 1;
            if last == Some(i.wrapping_sub(1)) {
                score += 5;
            }
            let start = i == 0
                || matches!(chars[i - 1], '/' | '_' | '-' | '.' | ' ')
                || (c.is_uppercase() && chars[i - 1].is_lowercase());
            if start {
                score += 8;
            }
            last = Some(i);
            at += 1;
        }
    }
    if at < query.len() {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let wanted: String = query.iter().collect();
    let stem = name.split('.').next().unwrap_or(&name);
    if name == wanted || stem == wanted {
        score += 60;
    } else if name.starts_with(&wanted) {
        score += 40;
    } else if name.contains(&wanted) {
        score += 25;
    }
    Some(score * 4 - chars.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mux-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project/src")).unwrap();
        std::fs::write(dir.join("project/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("secret.txt"), "keep out").unwrap();
        dir
    }

    #[test]
    fn keeps_reads_inside_the_root() {
        let dir = sandbox("contain");
        let root = dir.join("project");
        assert!(resolve(&root, "src/main.rs").is_ok());
        assert!(resolve(&root, "").is_ok());
        assert!(resolve(&root, "../secret.txt").is_err());
        assert!(resolve(&root, "src/../../secret.txt").is_err());
        assert!(resolve(&root, dir.join("secret.txt").to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("link.txt")).unwrap();
            std::os::unix::fs::symlink(&dir, root.join("up")).unwrap();
            assert!(resolve(&root, "link.txt").is_err());
            assert!(resolve(&root, "up/secret.txt").is_err());
            assert!(read_file(&root, "link.txt", 0, 100).is_err());
            assert!(list_dir(&root, "up").is_err());
        }
        let chunk = read_file(&root, "src/main.rs", 3, 4).unwrap();
        assert_eq!((chunk.total, chunk.bytes.as_slice()), (13, &b"main"[..]));
        assert_eq!(chunk.kind, FileKind::Text);
        let (entries, truncated) = list_dir(&root, "").unwrap();
        assert!(!truncated);
        assert_eq!(entries[0].name, "src");
        assert!(entries[0].dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tells_kinds_from_their_first_bytes() {
        assert_eq!(kind(b"\x89PNG\r\n\x1a\n\0\0", "a.bin"), FileKind::Image);
        assert_eq!(kind(b"\xff\xd8\xff\xe0", "photo"), FileKind::Image);
        assert_eq!(kind(b"RIFF\0\0\0\0WEBPVP8 ", "a"), FileKind::Image);
        assert_eq!(kind(b"%PDF-1.7\n", "doc.txt"), FileKind::Pdf);
        assert_eq!(kind(b"<svg xmlns=\"\">", "logo"), FileKind::Image);
        assert_eq!(kind(b"<?xml?><svg>", "logo.SVG"), FileKind::Image);
        assert_eq!(kind("héllo".as_bytes(), "a.md"), FileKind::Text);
        // cut partway through the é
        assert_eq!(kind(&"hé".as_bytes()[..2], "a.md"), FileKind::Text);
        assert_eq!(kind(b"\0\x01\0\0\0", "font.ttf"), FileKind::Binary);
        assert_eq!(kind(b"\xff\xfe\xfa", "a"), FileKind::Binary);
    }

    #[test]
    fn reads_git_marks_for_files_and_folders() {
        let out = b" M src/a.rs\0A  src/b.rs\0?? new/\0R  c.rs\0old.rs\0!! target/\0 D gone.rs\0";
        let marks = Marks::parse(out, "");
        assert_eq!(marks.mark("src/a.rs", false), Some('M'));
        assert_eq!(marks.mark("src/b.rs", false), Some('A'));
        assert_eq!(marks.mark("src", true), Some('M'));
        assert_eq!(marks.mark("new", true), Some('U'));
        assert_eq!(marks.mark("new/x.rs", false), Some('U'));
        assert_eq!(marks.mark("c.rs", false), Some('R'));
        assert_eq!(marks.mark("old.rs", false), None);
        assert_eq!(marks.mark("gone.rs", false), Some('D'));
        assert!(marks.ignored("target"));
        assert!(marks.ignored("target/debug"));
        assert!(!marks.ignored("src"));
        let nested = Marks::parse(b" M app/src/a.rs\0 M other.rs\0", "app/");
        assert_eq!(nested.mark("src/a.rs", false), Some('M'));
        assert_eq!(nested.changed.len(), 1);
    }

    #[test]
    fn ranks_fuzzy_matches_by_name_first() {
        let files: Vec<String> = [
            "crates/gui/src/main.rs",
            "crates/server/src/messages.rs",
            "docs/maintenance.md",
            "README.md",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(best(&files, "main", 10)[0], "crates/gui/src/main.rs");
        assert_eq!(best(&files, "msgs", 10), ["crates/server/src/messages.rs"]);
        assert_eq!(best(&files, "readme", 1), ["README.md"]);
        assert!(best(&files, "zzz", 10).is_empty());
        assert!(best(&files, "", 10).is_empty());
    }
}
