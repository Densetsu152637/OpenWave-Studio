//! The stdio side: what `openwave-studio mcp` runs. It connects to the app's socket (starting the
//! app first when none is running), presents the session token and then relays bytes between
//! the MCP client's stdio and the socket. The app does all the MCP work.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::endpoint;
use crate::listener::{AUTH_OK, AUTH_PREFIX};
use crate::secure;

/// How long to wait for the app to start and open its socket.
pub const LAUNCH_WAIT: Duration = Duration::from_secs(20);

/// Starts the app (detached) when none is running.
pub type Launcher = Box<dyn Fn() -> io::Result<()> + Send + Sync>;

/// What the relay needs.
pub struct ClientConfig {
    /// The `mcp` directory under the app's data directory.
    pub dir: PathBuf,
    /// How long to wait for a socket.
    pub wait: Duration,
    /// How to start the app; `None` never starts it.
    pub launch: Option<Launcher>,
}

/// Why no connection was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The app runs with "Allow AI control (MCP)" turned off.
    Disabled,
    /// The app could not be started.
    Launch(String),
    /// No working socket appeared in time; the text is the last problem seen.
    Timeout(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str(
                "OpenWave Studio is running with AI control turned off: enable \"Allow AI control (MCP)\" in its settings",
            ),
            Self::Launch(why) => write!(f, "could not start OpenWave Studio: {why}"),
            Self::Timeout(last) if last.is_empty() => {
                f.write_str("OpenWave Studio did not open its MCP socket in time")
            }
            Self::Timeout(last) => write!(
                f,
                "OpenWave Studio did not open its MCP socket in time ({last})"
            ),
        }
    }
}

impl std::error::Error for ConnectError {}

/// A byte stream to the app.
pub trait AppStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AppStream for T {}

/// Starts `exe` detached from this process (no console, own process group, outside any job
/// object that would kill it with this process) with no arguments. The app may hold unsaved
/// work, so it must not die just because the MCP client that started it went away.
pub fn launch_detached(exe: &Path) -> io::Result<()> {
    let mut command = Command::new(exe);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, and first also CREATE_BREAKAWAY_FROM_JOB,
        // which a job that does not allow breakaway refuses.
        const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;
        const BREAKAWAY: u32 = 0x0100_0000;
        if command.creation_flags(DETACHED | BREAKAWAY).spawn().is_ok() {
            return Ok(());
        }
        command.creation_flags(DETACHED);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map(|_| ())
}

/// Connects to the running app, starting it when it is not running, and authenticates.
pub async fn connect(config: &ClientConfig) -> Result<Box<dyn AppStream>, ConnectError> {
    let deadline = Instant::now() + config.wait;
    let mut launched = false;
    let mut last = String::new();
    loop {
        let file = endpoint::read(&config.dir).ok();
        if let Some(file) = &file {
            if !file.enabled && secure::process_alive(file.pid) {
                return Err(ConnectError::Disabled);
            }
            if file.enabled {
                match open_and_authenticate(&file.endpoint, &file.token).await {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last = error.to_string(),
                }
            }
        }
        let running = file
            .as_ref()
            .is_some_and(|file| secure::process_alive(file.pid));
        if !running
            && !launched
            && let Some(launch) = &config.launch
        {
            launch().map_err(|error| ConnectError::Launch(error.to_string()))?;
            launched = true;
        }
        if Instant::now() >= deadline {
            return Err(ConnectError::Timeout(last));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn open_and_authenticate(name: &str, token: &str) -> io::Result<Box<dyn AppStream>> {
    let stream = open(name).await?;
    authenticate(stream, token).await
}

/// Sends the token and waits for the app's verdict.
pub async fn authenticate(
    mut stream: Box<dyn AppStream>,
    token: &str,
) -> io::Result<Box<dyn AppStream>> {
    stream
        .write_all(format!("{AUTH_PREFIX}{token}\n").as_bytes())
        .await?;
    stream.flush().await?;
    // Read the verdict byte by byte so nothing after it is consumed.
    let mut verdict = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        if stream.read(&mut byte).await? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the app refused the session token",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        verdict.push(byte[0]);
        if verdict.len() > 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad handshake"));
        }
    }
    if verdict == AUTH_OK.as_bytes() {
        Ok(stream)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the app refused the session token",
        ))
    }
}

/// Opens the raw (unauthenticated) stream to an endpoint.
#[cfg(windows)]
pub async fn open(name: &str) -> io::Result<Box<dyn AppStream>> {
    use tokio::net::windows::named_pipe::ClientOptions;
    /// `ERROR_PIPE_BUSY`: every instance is in use for a moment.
    const PIPE_BUSY: i32 = 231;
    let mut attempts = 0;
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(Box::new(client)),
            Err(error) if error.raw_os_error() == Some(PIPE_BUSY) && attempts < 40 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Opens the raw (unauthenticated) stream to an endpoint.
#[cfg(unix)]
pub async fn open(path: &str) -> io::Result<Box<dyn AppStream>> {
    Ok(Box::new(tokio::net::UnixStream::connect(path).await?))
}

/// Runs the stdio relay until the app closes the connection or stdin ends. Returns the process
/// exit code; problems are written to stderr (stdout carries only protocol messages).
pub fn run_stdio(config: ClientConfig) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("openwave-studio mcp: {error}");
            return 1;
        }
    };
    let code = runtime.block_on(relay(&config));
    // A blocked stdin read must not hold the process up.
    runtime.shutdown_background();
    code
}

/// What a protocol line is, for counting requests that still wait for their answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    /// A message with a method and an id (an answer is due).
    Request,
    /// An answer to a request.
    Response,
    /// A notification or anything else.
    Other,
}

fn classify(line: &[u8]) -> Line {
    let Ok(serde_json::Value::Object(message)) = serde_json::from_slice(line) else {
        return Line::Other;
    };
    match (message.contains_key("method"), message.contains_key("id")) {
        (true, true) => Line::Request,
        (false, true) => Line::Response,
        _ => Line::Other,
    }
}

/// Splits a byte stream into lines and reports each complete one.
#[derive(Default)]
struct Lines(Vec<u8>);

impl Lines {
    fn feed(&mut self, bytes: &[u8], mut each: impl FnMut(&[u8])) {
        for &byte in bytes {
            if byte == b'\n' {
                each(&self.0);
                self.0.clear();
            } else {
                self.0.push(byte);
            }
        }
    }
}

async fn relay(config: &ClientConfig) -> i32 {
    let stream = match connect(config).await {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("openwave-studio mcp: {error}");
            return 1;
        }
    };
    let (mut from_app, mut to_app) = tokio::io::split(stream);
    let pending = Arc::new(AtomicIsize::new(0));
    let (eof_tx, mut eof_rx) = tokio::sync::oneshot::channel::<()>();
    let uplink = {
        let pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buffer = vec![0_u8; 16 * 1024];
            let mut lines = Lines::default();
            loop {
                let read = stdin.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                lines.feed(&buffer[..read], |line| {
                    if classify(line) == Line::Request {
                        pending.fetch_add(1, Ordering::AcqRel);
                    }
                });
                if to_app.write_all(&buffer[..read]).await.is_err() || to_app.flush().await.is_err()
                {
                    break;
                }
            }
            // The client closed its end: finish what it is still waiting for, then leave.
            let _ = eof_tx.send(());
        })
    };
    let mut stdout = tokio::io::stdout();
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut lines = Lines::default();
    let mut eof = false;
    let code = loop {
        if eof && pending.load(Ordering::Acquire) <= 0 {
            break 0;
        }
        let read = tokio::select! {
            read = from_app.read(&mut buffer) => read,
            _ = &mut eof_rx, if !eof => {
                eof = true;
                continue;
            }
        };
        match read {
            Ok(0) => break 0,
            Ok(read) => {
                lines.feed(&buffer[..read], |line| {
                    if classify(line) == Line::Response {
                        pending.fetch_sub(1, Ordering::AcqRel);
                    }
                });
                if stdout.write_all(&buffer[..read]).await.is_err() || stdout.flush().await.is_err()
                {
                    break 0;
                }
            }
            Err(error) => {
                eprintln!("openwave-studio mcp: connection to the app lost: {error}");
                break 1;
            }
        }
    };
    uplink.abort();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_classified_for_the_pending_count() {
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
            Line::Request
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Line::Response
        );
        assert_eq!(
            classify(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Line::Other
        );
        assert_eq!(classify(b"garbage"), Line::Other);
    }

    #[test]
    fn lines_are_split_across_chunks() {
        let mut seen = Vec::new();
        let mut lines = Lines::default();
        lines.feed(b"ab\ncd", |line| seen.push(line.to_vec()));
        lines.feed(b"ef\n", |line| seen.push(line.to_vec()));
        assert_eq!(seen, vec![b"ab".to_vec(), b"cdef".to_vec()]);
    }
}
