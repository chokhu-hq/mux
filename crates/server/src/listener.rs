// where clients connect: a unix socket where there is one, and a loopback port guarded by a secret
// on windows, or beside the socket when the app runs on another system, as windows does with a server in wsl
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;

use crate::{Event, connection};

pub struct Listener {
    #[cfg(unix)]
    unix: Option<(tokio::net::UnixListener, PathBuf)>,
    tcp: Option<Tcp>,
}

struct Tcp {
    listener: TcpListener,
    secret: Arc<String>,
    address: PathBuf,
}

pub enum Accepted {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(TcpStream),
}

impl Listener {
    #[cfg(unix)]
    pub fn bind(socket: &Path) -> io::Result<Self> {
        let unix = bind_unix(socket)?;
        // asked for by an app on another system, which can only reach this one through a port
        let tcp = match std::env::var_os("MUX_LISTEN") {
            Some(_) => Some(bind_tcp(
                &mux_core::protocol::data_dir().join("server.address"),
            )?),
            None => None,
        };
        Ok(Self {
            unix: Some((unix, socket.to_owned())),
            tcp,
        })
    }

    #[cfg(windows)]
    pub fn bind(socket: &Path) -> io::Result<Self> {
        Ok(Self {
            tcp: Some(bind_tcp(socket)?),
        })
    }

    // what the log says the server listens on
    pub fn describe(&self) -> String {
        let mut places = Vec::new();
        #[cfg(unix)]
        if let Some((_, path)) = &self.unix {
            places.push(path.display().to_string());
        }
        if let Some(tcp) = &self.tcp
            && let Ok(address) = tcp.listener.local_addr()
        {
            places.push(address.to_string());
        }
        places.join(" and ")
    }

    pub async fn accept(&self) -> io::Result<Accepted> {
        #[cfg(unix)]
        let unix = async {
            match &self.unix {
                Some((listener, _)) => listener.accept().await.map(|(s, _)| Accepted::Unix(s)),
                None => std::future::pending().await,
            }
        };
        #[cfg(not(unix))]
        let unix = std::future::pending::<io::Result<Accepted>>();
        let tcp = async {
            match &self.tcp {
                Some(tcp) => tcp.listener.accept().await.map(|(s, _)| Accepted::Tcp(s)),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            accepted = unix => accepted,
            accepted = tcp => accepted,
        }
    }

    pub fn serve(&self, accepted: Accepted, id: u64, events: UnboundedSender<Event>) {
        match accepted {
            #[cfg(unix)]
            Accepted::Unix(stream) => {
                tokio::spawn(connection(stream, id, events));
            }
            Accepted::Tcp(mut stream) => {
                let Some(secret) = self.tcp.as_ref().map(|t| t.secret.clone()) else {
                    return;
                };
                tokio::spawn(async move {
                    let proven =
                        tokio::time::timeout(Duration::from_secs(5), read_secret(&mut stream))
                            .await;
                    if matches!(proven, Ok(Some(given)) if given == *secret) {
                        connection(stream, id, events).await;
                    } else {
                        log::warn!("turned away a connection without the server's secret");
                    }
                });
            }
        }
    }

    // nothing should point at a server that has gone
    pub fn remove(&self) {
        #[cfg(unix)]
        if let Some((_, path)) = &self.unix {
            let _ = std::fs::remove_file(path);
        }
        if let Some(tcp) = &self.tcp {
            let _ = std::fs::remove_file(&tcp.address);
        }
    }
}

// the first line a port client sends, read a byte at a time so nothing after it is swallowed
async fn read_secret(stream: &mut TcpStream) -> Option<String> {
    let mut line = Vec::new();
    loop {
        let byte = stream.read_u8().await.ok()?;
        if byte == b'\n' {
            return String::from_utf8(line).ok();
        }
        if line.len() > 128 {
            return None;
        }
        line.push(byte);
    }
}

// a random port, written with a new secret into a file only this user can read
fn bind_tcp(address: &Path) -> io::Result<Tcp> {
    let running = mux_core::transport::read_address(address)
        .is_ok_and(|(port, _)| std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
    if running {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "a mux server is already running, as {} says",
                address.display()
            ),
        ));
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let secret = mux_core::transport::secret();
    mux_core::transport::write_address(address, port, &secret)?;
    Ok(Tcp {
        listener: TcpListener::from_std(listener)?,
        secret: Arc::new(secret),
        address: address.to_owned(),
    })
}

#[cfg(unix)]
fn bind_unix(path: &Path) -> io::Result<tokio::net::UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a mux server is already running on {}", path.display()),
            ));
        }
        // nobody answered, so this is a leftover from a server that did not shut down cleanly
        std::fs::remove_file(path)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    // the socket gives full control of the user's shells, so only the owner may connect
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
