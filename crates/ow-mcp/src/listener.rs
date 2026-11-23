//! The app's side of the transport: a per-user local socket every connection of which must
//! present the session token before anything else.
//!
//! * Windows: a named pipe created with a DACL for the current user only and
//!   `PIPE_REJECT_REMOTE_CLIENTS`; the first instance is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, so a pre-created pipe of that name makes listening fail
//!   instead of being used.
//! * Unix: a domain socket (mode 0600) in the 0700 `mcp` directory.
//!
//! Framing: after the token line, both directions carry one JSON-RPC message per line (the MCP
//! stdio framing). The first line the client sends is `OPENWAVE-MCP <token>`; the server answers
//! `OK` or `ERR refused` and closes. Wrong or missing tokens are logged to stderr.
//!
//! The listener runs on its own small tokio runtime inside a dedicated thread; tool calls run on
//! that runtime's blocking pool, never on an audio thread.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::watch;

use crate::endpoint::{self, EndpointFile, FORMAT_VERSION};
use crate::protocol::Server;
use crate::secure;

/// The first line of a connection, before the token.
pub const AUTH_PREFIX: &str = "OPENWAVE-MCP ";
/// The answer to a good token.
pub const AUTH_OK: &str = "OK";
/// The answer to a bad one.
pub const AUTH_REFUSED: &str = "ERR refused";

/// How long a new connection has to present its token.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest token line accepted.
const MAX_AUTH_LINE: usize = 256;
/// Longest message accepted (a large `edit` with many notes).
pub const MAX_LINE: usize = 32 * 1024 * 1024;
/// Connections served at once.
const MAX_CONNECTIONS: usize = 8;

/// A running listener. Dropping it stops it.
pub struct McpListener {
    dir: PathBuf,
    endpoint: String,
    token: String,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl McpListener {
    /// Starts listening and announces the endpoint in `dir` (the `mcp` directory). Fails when the
    /// pipe or socket cannot be created or the announcement cannot be written; nothing is left
    /// behind then.
    pub fn start(dir: &Path, server: Arc<Server>) -> io::Result<Self> {
        secure::create_private_dir(dir)?;
        let token = endpoint::new_token()?;
        let name = endpoint::new_endpoint(dir)?;
        let (stop, stop_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<io::Result<()>>();
        let thread = {
            let name = name.clone();
            let token: Arc<str> = Arc::from(token.as_str());
            std::thread::Builder::new()
                .name("openwave-mcp".into())
                .spawn(move || run(&name, token, server, stop_rx, ready_tx))?
        };
        let mut listener = Self {
            dir: dir.to_path_buf(),
            endpoint: name.clone(),
            token: token.clone(),
            stop,
            thread: Some(thread),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                listener.shut_down();
                return Err(error);
            }
            Err(_) => {
                listener.shut_down();
                return Err(io::Error::other("the MCP listener thread ended early"));
            }
        }
        let announced = endpoint::write(
            dir,
            &EndpointFile {
                version: FORMAT_VERSION,
                enabled: true,
                pid: std::process::id(),
                endpoint: name,
                token,
            },
        );
        if let Err(error) = announced {
            listener.shut_down();
            return Err(error);
        }
        Ok(listener)
    }

    /// The pipe name or socket path.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The session token (also in the announcement file).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Stops listening, closes every connection and removes the announcement.
    pub fn stop(mut self) {
        self.shut_down();
    }

    fn shut_down(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
            endpoint::remove(&self.dir);
        }
    }
}

impl Drop for McpListener {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Announces that AI control is off, so `openwave-studio mcp` does not start another app.
pub fn announce_disabled(dir: &Path) -> io::Result<()> {
    endpoint::write(
        dir,
        &EndpointFile {
            version: FORMAT_VERSION,
            enabled: false,
            pid: std::process::id(),
            endpoint: String::new(),
            token: String::new(),
        },
    )
}

fn run(
    name: &str,
    token: Arc<str>,
    server: Arc<Server>,
    stop: watch::Receiver<bool>,
    ready: std::sync::mpsc::Sender<io::Result<()>>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("openwave-mcp-io")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    runtime.block_on(async {
        let mut acceptor = match Acceptor::bind(name) {
            Ok(acceptor) => acceptor,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        accept_loop(&mut acceptor, token, server, stop).await;
    });
    // Connections that are still busy (a render in progress) get a moment, not forever.
    runtime.shutdown_timeout(Duration::from_secs(2));
}

async fn accept_loop(
    acceptor: &mut Acceptor,
    token: Arc<str>,
    server: Arc<Server>,
    mut stop: watch::Receiver<bool>,
) {
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = acceptor.accept() => match accepted {
                Ok(stream) => {
                    if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                        active.fetch_sub(1, Ordering::AcqRel);
                        eprintln!("[openwave] MCP connection refused: too many connections");
                        continue;
                    }
                    let (token, server, stop, active) =
                        (Arc::clone(&token), Arc::clone(&server), stop.clone(), Arc::clone(&active));
                    tokio::spawn(async move {
                        serve_connection(stream, &token, server, stop).await;
                        active.fetch_sub(1, Ordering::AcqRel);
                    });
                }
                Err(error) => {
                    eprintln!("[openwave] MCP accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// True when `line` is the auth line carrying `token` (compared in constant time).
fn authenticated(line: &str, token: &str) -> bool {
    let Some(presented) = line.strip_prefix(AUTH_PREFIX) else {
        return false;
    };
    constant_time_eq(presented.as_bytes(), token.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0_u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

async fn serve_connection<S>(
    stream: S,
    token: &str,
    server: Arc<Server>,
    mut stop: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let first = tokio::time::timeout(AUTH_TIMEOUT, read_line(&mut reader, MAX_AUTH_LINE)).await;
    let good = matches!(&first, Ok(Ok(Some(line))) if authenticated(line, token));
    if !good {
        eprintln!("[openwave] MCP connection refused: missing or wrong session token");
        let _ = write
            .write_all(format!("{AUTH_REFUSED}\n").as_bytes())
            .await;
        let _ = write.shutdown().await;
        return;
    }
    if write
        .write_all(format!("{AUTH_OK}\n").as_bytes())
        .await
        .is_err()
    {
        return;
    }
    loop {
        let line = tokio::select! {
            _ = stop.changed() => break,
            line = read_line(&mut reader, MAX_LINE) => line,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                eprintln!("[openwave] MCP connection closed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let server = Arc::clone(&server);
        let Ok(answer) = tokio::task::spawn_blocking(move || server.handle_line(&line)).await
        else {
            eprintln!("[openwave] an MCP tool panicked; closing the connection");
            break;
        };
        if let Some(mut answer) = answer {
            answer.push('\n');
            if write.write_all(answer.as_bytes()).await.is_err() || write.flush().await.is_err() {
                break;
            }
        }
    }
    let _ = write.shutdown().await;
}

/// Reads one line (without its line ending), at most `max` bytes long. `None` at end of stream.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<String>> {
    let mut buffer = Vec::new();
    let read = (&mut *reader)
        .take(max as u64 + 1)
        .read_until(b'\n', &mut buffer)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if buffer.last() != Some(&b'\n') && buffer.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a message is longer than the limit",
        ));
    }
    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
        buffer.pop();
    }
    String::from_utf8(buffer)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a message is not UTF-8"))
}

#[cfg(windows)]
struct Acceptor {
    name: String,
    security: secure::PipeSecurity,
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
#[allow(unsafe_code)]
impl Acceptor {
    fn create(
        name: &str,
        security: &secure::PipeSecurity,
        first: bool,
    ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: `security.as_ptr()` points to SECURITY_ATTRIBUTES (with a valid descriptor)
        // that `security` keeps alive and unchanged for the duration of the call.
        unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) }
    }

    fn bind(name: &str) -> io::Result<Self> {
        let security = secure::PipeSecurity::current_user()?;
        let next = Self::create(name, &security, true)?;
        Ok(Self {
            name: name.to_owned(),
            security,
            next,
        })
    }

    async fn accept(&mut self) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        self.next.connect().await?;
        let fresh = Self::create(&self.name, &self.security, false)?;
        Ok(std::mem::replace(&mut self.next, fresh))
    }
}

#[cfg(unix)]
struct Acceptor(tokio::net::UnixListener);

#[cfg(unix)]
impl Acceptor {
    fn bind(path: &str) -> io::Result<Self> {
        // A socket file left by a crashed run would make `bind` fail.
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path)?;
        secure::restrict_file(Path::new(path))?;
        Ok(Self(listener))
    }

    async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
        self.0.accept().await.map(|(stream, _)| stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_token_authenticates() {
        let token = "ab".repeat(32);
        assert!(authenticated(&format!("{AUTH_PREFIX}{token}"), &token));
        assert!(!authenticated(
            &format!("{AUTH_PREFIX}{}", "cd".repeat(32)),
            &token
        ));
        assert!(!authenticated(
            &format!("{AUTH_PREFIX}{}", &token[..63]),
            &token
        ));
        assert!(!authenticated(&token, &token));
        assert!(!authenticated("", &token));
    }

    #[tokio::test]
    async fn lines_are_bounded_and_stripped() {
        let mut data: &[u8] = b"hello\r\nworld\nlast";
        assert_eq!(
            read_line(&mut data, 16).await.unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(
            read_line(&mut data, 16).await.unwrap().as_deref(),
            Some("world")
        );
        assert_eq!(
            read_line(&mut data, 16).await.unwrap().as_deref(),
            Some("last")
        );
        assert_eq!(read_line(&mut data, 16).await.unwrap(), None);
        let mut long: &[u8] = b"0123456789abcdef0123\n";
        assert!(read_line(&mut long, 8).await.is_err());
    }
}
