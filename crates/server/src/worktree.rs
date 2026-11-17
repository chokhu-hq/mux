use std::path::Path;

use mux_core::protocol;

// a new checkout of the project on its own branch, kept with mux's data so the project folder stays tidy
pub fn add(repo: &str, title: &str, id: u64) -> Result<String, String> {
    add_under(&protocol::data_dir().join("worktrees"), repo, title, id)
}

// a checkout left on no branch, for `gh pr checkout` to put the pull request's branch in
pub fn add_detached(repo: &str, title: &str, id: u64) -> Result<String, String> {
    add_at(
        &protocol::data_dir().join("worktrees"),
        repo,
        title,
        id,
        false,
    )
}

fn add_under(root: &Path, repo: &str, title: &str, id: u64) -> Result<String, String> {
    add_at(root, repo, title, id, true)
}

fn add_at(root: &Path, repo: &str, title: &str, id: u64, branch: bool) -> Result<String, String> {
    let slug = slug(title);
    let name = Path::new(repo).file_name().map_or_else(
        || "project".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let dir = root.join(name).join(format!("{slug}-{id}"));
    let branch = branch.then(|| format!("mux/{slug}-{id}"));
    let mut args: Vec<&std::ffi::OsStr> = vec!["worktree".as_ref(), "add".as_ref()];
    match &branch {
        Some(branch) => args.extend::<[&std::ffi::OsStr; 2]>(["-b".as_ref(), branch.as_ref()]),
        None => args.push("--detach".as_ref()),
    }
    args.extend([dir.as_os_str(), "HEAD".as_ref()]);
    if let Err(reason) = mux_core::git(mux_core::command("git"), repo, args) {
        return Err(if reason.contains("not a git repository") {
            "This project isn't a git repository, so the agent can't get its own worktree. Switch it to Local to work in the folder itself.".to_owned()
        } else if reason.contains("invalid reference: HEAD") {
            "This project has no initial commit, so the agent can't get its own worktree. Make a commit first or switch it to Local.".to_owned()
        } else if reason.starts_with("couldn't run git") {
            format!("Couldn't run git to make a worktree: {reason}")
        } else {
            format!(
                "Couldn't make a worktree: {}",
                reason.trim_start_matches("fatal: ")
            )
        });
    }
    Ok(dir.to_string_lossy().into_owned())
}

// fetches the pull request and switches the checkout to its branch, the way gh sets it up to push back
pub fn checkout_pr(dir: &str, number: u64) -> Result<(), String> {
    let output = mux_core::command("gh")
        .args(["pr", "checkout", &number.to_string()])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("couldn't run gh: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("gh failed without saying why")
        .to_owned())
}

// git refuses to remove a checkout with uncommitted work, which is the point, and the branch always stays
pub fn remove(path: String) {
    std::thread::spawn(move || {
        let _ = mux_core::git(
            mux_core::command("git"),
            &path,
            ["worktree", "remove", &path],
        );
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

    fn repository() -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "mux-worktree-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let hooks = root.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        git(
            &repo,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        (root, repo)
    }

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        let output = mux_core::command("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

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
        let (root, repo) = repository();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        std::fs::write(repo.join("a.txt"), "uncommitted").unwrap();
        let trees = root.join("worktrees");
        let path = add_under(&trees, &repo.to_string_lossy(), "Add a feature", 7).unwrap();
        assert_eq!(
            std::fs::read_to_string(Path::new(&path).join("a.txt")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "uncommitted"
        );
        let branches = String::from_utf8_lossy(&git(&repo, &["branch"]).stdout).into_owned();
        assert!(branches.contains("mux/add-a-feature-7"));
        git(&repo, &["worktree", "remove", &path]);
        assert!(!Path::new(&path).exists());
        let outside = root.join("not-a-repo");
        std::fs::create_dir_all(&outside).unwrap();
        let refused = add_under(&trees, &outside.to_string_lossy(), "x", 1).unwrap_err();
        assert!(refused.contains("isn't a git repository"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn makes_a_detached_worktree_for_a_pull_request() {
        let (root, repo) = repository();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let trees = root.join("worktrees");
        let path = add_at(&trees, &repo.to_string_lossy(), "pr 12", 3, false).unwrap();
        assert!(path.ends_with("pr-12-3"));
        assert!(Path::new(&path).join("a.txt").is_file());
        let branches = String::from_utf8_lossy(&git(&repo, &["branch"]).stdout).into_owned();
        assert!(!branches.contains("mux/"), "{branches}");
        git(&repo, &["worktree", "remove", &path]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_a_repository_without_an_initial_commit() {
        let (root, repo) = repository();
        std::fs::write(repo.join("a.txt"), "uncommitted").unwrap();
        let refused = add_under(&root.join("worktrees"), &repo.to_string_lossy(), "Start", 1);
        assert!(
            refused.is_err(),
            "an unborn branch must not produce an empty checkout: {refused:?}"
        );
        assert!(refused.unwrap_err().contains("initial commit"));
        assert!(!root.join("worktrees/repo/start-1").exists());
        assert!(
            String::from_utf8_lossy(&git(&repo, &["branch"]).stdout)
                .trim()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
