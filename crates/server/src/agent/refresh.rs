// claude's sign-in tokens work once, so a claude killed while it swaps them for new ones loses the new pair
// and the user is signed out. it holds a lock while it does that, and a claude is only stopped once it's let go
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use tokio::process::Child;

// claude touches its lock every few seconds while it refreshes, so an older one was left by a claude that died
const LIVE: Duration = Duration::from_secs(20);
// long enough for claude's refresh request to time out and the result to be saved
const PATIENCE: Duration = Duration::from_secs(40);

pub(crate) async fn stop_claude(child: Child) {
    refresh_done().await;
    drop(child);
}

// once no claude is in the middle of refreshing the sign-in, or it's been long enough to give up waiting
pub(crate) async fn refresh_done() {
    settled(&config_dir()).await;
}

async fn settled(config: &Path) {
    let locks = lock_paths(config);
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && locks.iter().any(|lock| held(lock)) {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn held(lock: &Path) -> bool {
    std::fs::metadata(lock)
        .and_then(|m| m.modified())
        .is_ok_and(|at| SystemTime::now().duration_since(at).unwrap_or_default() < LIVE)
}

// the lock inside claude's config folder, and the one beside it that older versions take
fn lock_paths(config: &Path) -> [PathBuf; 2] {
    let real = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_owned());
    let mut legacy = real.into_os_string();
    legacy.push(".lock");
    [config.join(".oauth_refresh.lock"), legacy.into()]
}

// where claude keeps its sign-in, which can be moved with either of these
fn config_dir() -> PathBuf {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    var("CLAUDE_SECURESTORAGE_CONFIG_DIR")
        .or_else(|| var("CLAUDE_CONFIG_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| mux_core::home_dir().join(".claude"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_a_refresh_in_progress_but_not_for_a_stale_lock() {
        let dir = std::env::temp_dir().join(format!("mux-refresh-{}", std::process::id()));
        let config = dir.join(".claude");
        std::fs::create_dir_all(&config).unwrap();
        let lock = config.join(".oauth_refresh.lock");

        let started = Instant::now();
        settled(&config).await;
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "no lock, no wait"
        );

        std::fs::create_dir(&lock).unwrap();
        let release = {
            let lock = lock.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                std::fs::remove_dir(lock).unwrap();
            })
        };
        let started = Instant::now();
        settled(&config).await;
        assert!(
            started.elapsed() >= Duration::from_millis(350),
            "waited for the refresh"
        );
        release.await.unwrap();

        std::fs::create_dir(&lock).unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let started = Instant::now();
        settled(&config).await;
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a dead claude's lock holds nothing up"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
