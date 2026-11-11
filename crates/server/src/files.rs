use std::path::Path;

// enough for big repos without sending the client a huge list
const LIMIT: usize = 20_000;

// the project's files for `@` mentions, relative and with forward slashes, honouring .gitignore where there is one
pub fn list(dir: &Path) -> Vec<String> {
    let output = mux_core::command("git")
        .arg("-C")
        .arg(dir)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let mut files: Vec<String> = output
            .stdout
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
