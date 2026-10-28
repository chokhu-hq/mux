// how the app reaches its server: a unix socket where there is one, and on windows a loopback port
// guarded by a secret, both kept in a file only this user can read
use std::io;
use std::path::Path;

#[cfg(unix)]
pub type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type Stream = std::net::TcpStream;

#[cfg(unix)]
pub fn connect(path: &Path) -> io::Result<Stream> {
    Stream::connect(path)
}

#[cfg(windows)]
pub fn connect(path: &Path) -> io::Result<Stream> {
    use std::io::Write;
    let (port, secret) = read_address(path)?;
    let mut stream = Stream::connect(("127.0.0.1", port))?;
    stream.write_all(format!("{secret}\n").as_bytes())?;
    Ok(stream)
}

// the address file holds the port and the secret a client proves itself with, as `port secret`
pub fn read_address(path: &Path) -> io::Result<(u16, String)> {
    parse_address(&std::fs::read_to_string(path)?)
}

pub fn parse_address(text: &str) -> io::Result<(u16, String)> {
    let mut parts = text.split_whitespace();
    let port = parts.next().and_then(|p| p.parse().ok());
    let secret = parts.next().filter(|s| !s.is_empty());
    match (port, secret) {
        (Some(port), Some(secret)) => Ok((port, secret.to_owned())),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the server's address file is unreadable",
        )),
    }
}

pub fn write_address(path: &Path, port: u16, secret: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    // the secret is all that stands between another account and this user's shells, so it's private from the start
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    options
        .open(path)?
        .write_all(format!("{port} {secret}\n").as_bytes())
}

// each RandomState is keyed from the os's random source, enough for a secret that never leaves this machine
pub fn secret() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..4u64)
        .map(|i| {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_files_round_trip() {
        let path = std::env::temp_dir().join(format!("mux-address-{}", std::process::id()));
        let secret = secret();
        assert_eq!(secret.len(), 64);
        assert_ne!(secret, super::secret());
        write_address(&path, 49152, &secret).unwrap();
        assert_eq!(read_address(&path).unwrap(), (49152, secret));
        std::fs::write(&path, "nonsense").unwrap();
        assert!(read_address(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
