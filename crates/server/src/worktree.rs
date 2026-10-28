use std::path::Path;
use std::process::Command;

use mux_core::protocol;

// a new checkout of the project on its own branch, kept with mux's data so the project folder stays tidy
pub fn add(repo: &str, title: &str, id: u64) -> Result<String, String> {
    add_under(&protocol::data_dir().join("worktrees"), repo, title, id)
}

fn add_under(root: &Path, repo: &str, title: &str, id: u64) -> Result<String, String> {
    let slug = slug(title);
    let name = Path::new(repo).file_name().map_or_else(
        || "project".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let dir = root.join(name).join(format!("{slug}-{id}"));
    let branch = format!("mux/{slug}-{id}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "-b", &branch])
        .arg(&dir)
        .output()
        .map_err(|err| format!("Couldn't run git to make a worktree: {err}"))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default();
        return Err(if reason.contains("not a git repository") {
            "This project isn't a git repository, so the agent can't get its own worktree. Switch it to Local to work in the folder itself.".to_owned()
        } else {
            format!(
                "Couldn't make a worktree: {}",
                reason.trim_start_matches("fatal: ")
            )
        });
    }
    Ok(dir.to_string_lossy().into_owned())
}

// git refuses to remove a checkout with uncommitted work, which is the point, and the branch always stays
pub fn remove(path: String) {
    std::thread::spawn(move || {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["worktree", "remove"])
            .arg(&path)
            .output();
    });
}

// a branch friendly version of a title, like `fix-the-flaky-test`
fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let mut slug = String::new();
    for word in words {
        if slug.len() + word.len() > 32 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word);
    }
    if slug.is_empty() {
        "agent".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_short_branch_names() {
        assert_eq!(
            slug("Fix the flaky session test!"),
            "fix-the-flaky-session-test"
        );
        assert_eq!(slug("…"), "agent");
        assert!(slug(&"word ".repeat(30)).len() <= 32);
    }

    #[test]
    fn makes_and_removes_a_worktree_on_its_own_branch() {
        let root = std::env::temp_dir().join(format!("mux-worktree-{}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let trees = root.join("worktrees");
        let path = add_under(&trees, &repo.to_string_lossy(), "Add a feature", 7).unwrap();
        assert!(Path::new(&path).join("a.txt").exists());
        let branches = String::from_utf8_lossy(&git(&["branch"]).stdout).into_owned();
        assert!(branches.contains("mux/add-a-feature-7"));
        let outside = std::env::temp_dir().join(format!("mux-not-a-repo-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let refused = add_under(&trees, &outside.to_string_lossy(), "x", 1).unwrap_err();
        assert!(refused.contains("isn't a git repository"));
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
