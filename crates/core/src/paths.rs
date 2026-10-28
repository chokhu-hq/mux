// the server can run in wsl while the app runs on windows, and then each side names files its own way:
// the server says `/home/me/code`, which windows opens as `\\wsl.localhost\Ubuntu\home\me\code`
use std::sync::RwLock;

static WSL: RwLock<Option<String>> = RwLock::new(None);

// the wsl distribution the server runs in, or none when it runs beside the app
pub fn set_wsl(distro: Option<String>) {
    if let Ok(mut wsl) = WSL.write() {
        *wsl = distro;
    }
}

pub fn wsl() -> Option<String> {
    WSL.read().ok().and_then(|wsl| wsl.clone())
}

// a path from the server, as this app can open it
pub fn local(path: &str) -> String {
    match wsl() {
        Some(distro) => to_windows(path, &distro),
        None => path.to_owned(),
    }
}

// a path picked in this app, as the server sees it
pub fn remote(path: &str) -> String {
    match wsl() {
        Some(distro) => to_linux(path, &distro),
        None => path.to_owned(),
    }
}

// a program run where the server runs, so tools like git see the same files the agent does
pub fn command(program: &str) -> std::process::Command {
    match wsl() {
        Some(distro) => {
            let mut command = crate::command("wsl.exe");
            // --exec skips the default shell, which would otherwise reinterpret paths with spaces or quotes
            command.args(["-d", &distro, "--exec", program]);
            command
        }
        None => crate::command(program),
    }
}

// a path made absolute against a folder, where a linux path counts as absolute even when windows doesn't think so
pub fn join(folder: &str, path: &str) -> String {
    if path.starts_with('/') || std::path::Path::new(path).is_absolute() {
        return path.to_owned();
    }
    if folder.starts_with('/') {
        return format!("{}/{path}", folder.trim_end_matches('/'));
    }
    std::path::Path::new(folder)
        .join(path)
        .to_string_lossy()
        .into_owned()
}

// the folder a file is in, read the way the path is written
pub fn parent(path: &str) -> String {
    if path.starts_with('/') {
        return match path.rsplit_once('/') {
            Some(("", _)) => "/".to_owned(),
            Some((folder, _)) => folder.to_owned(),
            None => path.to_owned(),
        };
    }
    std::path::Path::new(path)
        .parent()
        .map_or_else(|| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

// windows drives are mounted under /mnt in wsl, and the rest of linux is shared as a network path
pub fn to_windows(path: &str, distro: &str) -> String {
    if !path.starts_with('/') {
        return path.to_owned();
    }
    let mut parts = path[1..].splitn(3, '/');
    if parts.next() == Some("mnt")
        && let Some(drive) = parts.next()
        && drive.len() == 1
        && drive.chars().all(|c| c.is_ascii_alphabetic())
    {
        let rest = parts.next().unwrap_or_default().replace('/', "\\");
        return format!("{}:\\{rest}", drive.to_ascii_uppercase());
    }
    format!("\\\\wsl.localhost\\{distro}{}", path.replace('/', "\\"))
}

pub fn to_linux(path: &str, distro: &str) -> String {
    let slashed = path.replace('\\', "/");
    for host in ["//wsl.localhost/", "//wsl$/"] {
        if let Some(rest) = strip_prefix_ignore_case(&slashed, host)
            && let Some(inside) = strip_prefix_ignore_case(rest, distro)
            && (inside.is_empty() || inside.starts_with('/'))
        {
            return if inside.is_empty() {
                "/".to_owned()
            } else {
                inside.to_owned()
            };
        }
    }
    let bytes = slashed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = slashed[2..].trim_start_matches('/');
        return if rest.is_empty() {
            format!("/mnt/{drive}")
        } else {
            format!("/mnt/{drive}/{rest}")
        };
    }
    slashed
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|start| start.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_paths_open_through_the_wsl_share() {
        assert_eq!(
            to_windows("/home/me/code/a.rs", "Ubuntu"),
            r"\\wsl.localhost\Ubuntu\home\me\code\a.rs"
        );
        assert_eq!(
            to_windows("/mnt/c/Users/me/a.png", "Ubuntu"),
            r"C:\Users\me\a.png"
        );
        assert_eq!(
            to_windows("/mnt/data/x", "Ubuntu"),
            r"\\wsl.localhost\Ubuntu\mnt\data\x"
        );
        assert_eq!(to_windows("relative/a.rs", "Ubuntu"), "relative/a.rs");
    }

    #[test]
    fn windows_paths_reach_wsl_through_mnt() {
        assert_eq!(
            to_linux(r"C:\Users\me\Pictures\a.png", "Ubuntu"),
            "/mnt/c/Users/me/Pictures/a.png"
        );
        assert_eq!(to_linux(r"D:\", "Ubuntu"), "/mnt/d");
        assert_eq!(
            to_linux(r"\\wsl.localhost\Ubuntu\home\me\code", "Ubuntu"),
            "/home/me/code"
        );
        assert_eq!(to_linux(r"\\wsl$\ubuntu\home\me", "Ubuntu"), "/home/me");
        assert_eq!(to_linux(r"\\wsl.localhost\Ubuntu", "Ubuntu"), "/");
        // another distribution's files aren't this server's to see
        assert_eq!(
            to_linux(r"\\wsl.localhost\Debian\home", "Ubuntu"),
            "//wsl.localhost/Debian/home"
        );
    }

    #[test]
    fn joins_and_parents_keep_linux_paths_whole() {
        assert_eq!(join("/home/me/proj", "src/a.rs"), "/home/me/proj/src/a.rs");
        assert_eq!(join("/home/me/proj/", "/etc/hosts"), "/etc/hosts");
        assert_eq!(parent("/home/me/proj/a.rs"), "/home/me/proj");
        assert_eq!(parent("/a.rs"), "/");
    }

    #[test]
    fn round_trips_keep_the_path() {
        for path in ["/home/me/code/mux", "/mnt/c/Users/me", "/etc/hosts"] {
            assert_eq!(to_linux(&to_windows(path, "Ubuntu"), "Ubuntu"), path);
        }
    }
}
