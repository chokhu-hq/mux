use std::sync::mpsc;

// the agent knows what it changed and how this repository words its commits, so it writes the message
pub(crate) const COMMIT: &str = "Commit the changes you made in this worktree. Stage only the files you changed, and write the commit message in the style of this repository's recent commits.";

// a worktree chat whose diffs are open, with any push under way
pub(crate) struct Ship {
    pub id: u64,
    pub worktree: String,
    pub pushing: Option<mpsc::Receiver<Result<Pushed, String>>>,
    pub error: Option<String>,
}

impl Ship {
    pub fn new(id: u64, worktree: String) -> Self {
        Self {
            id,
            worktree,
            pushing: None,
            error: None,
        }
    }
}

// what a push led to on github
pub(crate) enum Pushed {
    Pr(mux_core::protocol::PrLink),
    // the page for opening one by hand, when gh couldn't
    Compare(String),
    // the remote isn't on github
    Nothing,
}

// pushes the worktree's branch, then opens a pull request for it with gh or falls back to github's page for one
pub(crate) fn push(
    worktree: String,
    ctx: eframe::egui::Context,
) -> mpsc::Receiver<Result<Pushed, String>> {
    crate::in_background(&ctx, move || push_now(&worktree))
}

fn push_now(worktree: &str) -> Result<Pushed, String> {
    if !git(worktree, &["status", "--porcelain"])?.trim().is_empty() {
        return Err("There are changes that aren't committed yet. Commit them first.".to_owned());
    }
    let branch = git(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    git(worktree, &["push", "--set-upstream", "origin", branch])?;
    let remote = git(worktree, &["remote", "get-url", "origin"])?;
    let Some(compare) = compare_url(remote.trim(), branch) else {
        return Ok(Pushed::Nothing);
    };
    Ok(match crate::pr::open_for(worktree, branch) {
        Some(link) => Pushed::Pr(link),
        None => Pushed::Compare(compare),
    })
}

// git runs where the server does, so it reads the same checkout the agent changed
fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    mux_core::git(mux_core::paths::command("git"), dir, args)
        .map(|out| String::from_utf8_lossy(&out).into_owned())
        .map_err(|reason| format!("git {}: {reason}", args[0]))
}

// github's compare page for a branch, from any of the ways an origin on github is written
fn compare_url(remote: &str, branch: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!(
        "https://github.com/{owner}/{repo}/compare/{branch}?expand=1"
    ))
}

#[cfg(test)]
mod tests {
    use super::compare_url;

    #[test]
    fn github_remotes_become_a_compare_page() {
        let want = Some("https://github.com/me/app/compare/mux/fix-1?expand=1".to_owned());
        for remote in [
            "git@github.com:me/app.git",
            "git@github.com:me/app",
            "ssh://git@github.com/me/app.git",
            "https://github.com/me/app.git",
            "https://github.com/me/app/",
        ] {
            assert_eq!(compare_url(remote, "mux/fix-1"), want, "{remote}");
        }
    }

    #[test]
    fn other_hosts_get_no_page() {
        assert_eq!(compare_url("git@gitlab.com:me/app.git", "b"), None);
        assert_eq!(compare_url("https://github.com/me", "b"), None);
        assert_eq!(compare_url("/srv/git/app.git", "b"), None);
    }
}
