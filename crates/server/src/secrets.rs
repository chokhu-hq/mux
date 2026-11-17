use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mux_core::protocol::{McpServer, McpTransport, McpValue};

// each mcp env and header value lives in its own owner-only file, so state.json and clients only see ids
pub fn folder() -> PathBuf {
    mux_core::protocol::data_dir().join("secrets")
}

// the value an agent's config gets
pub fn reveal(value: &McpValue) -> String {
    reveal_in(&folder(), value)
}

fn reveal_in(dir: &Path, value: &McpValue) -> String {
    match value {
        McpValue::Plain(text) => text.clone(),
        McpValue::Saved(id) => std::fs::read_to_string(dir.join(id)).unwrap_or_else(|err| {
            log::warn!("couldn't read mcp secret {id}: {err}");
            String::new()
        }),
    }
}

fn values(servers: &mut [McpServer]) -> impl Iterator<Item = &mut Vec<(String, McpValue)>> {
    servers.iter_mut().map(|s| match &mut s.transport {
        McpTransport::Stdio { env, .. } => env,
        McpTransport::Http { headers, .. } => headers,
    })
}

fn saved_ids(servers: &[McpServer]) -> Vec<String> {
    servers
        .iter()
        .flat_map(|s| match &s.transport {
            McpTransport::Stdio { env, .. } => env,
            McpTransport::Http { headers, .. } => headers,
        })
        .filter_map(|(_, v)| match v {
            McpValue::Saved(id) => Some(id.clone()),
            McpValue::Plain(_) => None,
        })
        .collect()
}

fn save_in(dir: &Path, text: &str) -> io::Result<String> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    mux_core::private_dir(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let id = format!(
        "{nanos:x}{:x}{:x}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    );
    // written then renamed, so a crash never leaves half a key behind
    let tmp = dir.join(format!("{id}.tmp"));
    mux_core::write_private(&tmp, text.as_bytes())?;
    std::fs::rename(tmp, dir.join(&id))?;
    Ok(id)
}

// moves values typed in or left in state.json by older versions into their own files
pub fn store_in(dir: &Path, servers: &mut [McpServer]) {
    for list in values(servers) {
        for (_, value) in list.iter_mut() {
            if let McpValue::Plain(text) = value {
                match save_in(dir, text) {
                    Ok(id) => *value = McpValue::Saved(id),
                    Err(err) => {
                        log::error!("couldn't save an mcp secret in {}: {err}", dir.display())
                    }
                }
            }
        }
    }
}

// a client's new list, where a saved id it sent back keeps the stored value and anything typed is stored.
// ids it didn't have go, since a client can only name the values it was sent, and dropped values are deleted
pub fn replace(servers: &mut [McpServer], old: &[McpServer]) {
    replace_in(&folder(), servers, old);
}

fn replace_in(dir: &Path, servers: &mut [McpServer], old: &[McpServer]) {
    let known = saved_ids(old);
    for list in values(servers) {
        list.retain(|(_, v)| match v {
            McpValue::Saved(id) => known.contains(id),
            McpValue::Plain(_) => true,
        });
    }
    store_in(dir, servers);
    let kept = saved_ids(servers);
    for id in known.iter().filter(|id| !kept.contains(id)) {
        let _ = std::fs::remove_file(dir.join(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(env: Vec<(&str, McpValue)>) -> McpServer {
        McpServer {
            name: "probe".to_owned(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "x".to_owned(),
                args: Vec::new(),
                env: env.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            },
        }
    }

    fn env(server: &McpServer) -> &[(String, McpValue)] {
        match &server.transport {
            McpTransport::Stdio { env, .. } => env,
            McpTransport::Http { headers, .. } => headers,
        }
    }

    #[test]
    fn saved_values_are_kept_replaced_or_deleted() {
        let dir = std::env::temp_dir().join(format!("mux-secrets-{}", std::process::id()));
        let plain = |s: &str| McpValue::Plain(s.to_owned());
        let mut old = vec![server(vec![("A", plain("one")), ("B", plain("two"))])];
        store_in(&dir, &mut old);
        let [(_, a), (_, b)] = env(&old[0]) else {
            panic!("expected two values");
        };
        assert_eq!(reveal_in(&dir, a), "one");
        let McpValue::Saved(b_id) = b.clone() else {
            panic!("expected b saved");
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!((mode(&dir), mode(&dir.join(&b_id))), (0o700, 0o600));
        }

        // a keeps its placeholder, b is dropped, c is typed in and a stranger's id is ignored
        let mut new = vec![server(vec![
            ("A", a.clone()),
            ("C", plain("three")),
            ("D", McpValue::Saved("../state.json".to_owned())),
        ])];
        replace_in(&dir, &mut new, &old);
        let values = env(&new[0]);
        assert_eq!(values.len(), 2);
        assert_eq!(&values[0].1, a);
        assert_eq!(reveal_in(&dir, &values[0].1), "one");
        assert!(matches!(&values[1].1, McpValue::Saved(_)));
        assert_eq!(reveal_in(&dir, &values[1].1), "three");
        assert!(!dir.join(b_id).exists(), "a removed value's file goes");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
