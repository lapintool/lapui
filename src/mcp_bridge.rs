//! Authenticated loopback bridge for attaching a fresh stdio MCP client to an
//! already-running Lapui window.

use crate::{action::ActionRegistry, mcp::LapuiMcpServer, reload::ReloadHandle};
use rmcp::{transport::async_rw::AsyncRwTransport, RoleServer, ServiceExt};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::TcpListener as StdTcpListener,
    path::PathBuf,
    sync::Arc,
    thread,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, Semaphore},
};

const MAGIC: &str = "LAPUI-MCP/1";
const MAX_CONNECTIONS: usize = 4;
const MAX_AUTH_LINE: usize = 96;

#[derive(Debug, Serialize, Deserialize)]
struct Descriptor {
    address: String,
    token: String,
}

struct DescriptorLease {
    path: PathBuf,
    token: String,
}

pub struct McpBridgeHandle {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for McpBridgeHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DescriptorLease {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Descriptor>(&text).ok())
            .is_some_and(|descriptor| descriptor.token == self.token)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("bridge id must contain 1..64 ASCII letters, digits, '-' or '_'".into());
    }
    Ok(())
}

fn bridge_directory() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is not set")?;
    #[cfg(not(windows))]
    let base = if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime)
    } else {
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?)
            .join(".local")
            .join("state")
    };

    let directory = base.join("lapui").join("mcp-bridge");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(directory)
}

fn descriptor_path(id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    Ok(bridge_directory()?.join(format!("{id}.json")))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Bind an ephemeral loopback-only listener and publish a per-user capability
/// file. The stdio MCP adapter can reconnect through this bridge without
/// restarting the window or repeating application actions.
pub fn start(
    reload: ReloadHandle,
    actions: ActionRegistry,
    id: &str,
) -> Result<McpBridgeHandle, String> {
    let path = descriptor_path(id)?;
    let listener = StdTcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let descriptor = Descriptor {
        address: listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .to_string(),
        token: random_token()?,
    };
    let serialized = serde_json::to_vec(&descriptor).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| {
        format!(
            "cannot create MCP bridge descriptor {}: {error}; remove a stale file only after confirming no Lapui instance uses this id",
            path.display()
        )
    })?;
    if let Err(error) = file.write_all(&serialized).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error.to_string());
    }
    drop(file);

    let lease = DescriptorLease {
        path,
        token: descriptor.token.clone(),
    };
    let (shutdown, shutdown_rx) = oneshot::channel();
    let thread = thread::Builder::new()
        .name("lapui-mcp-bridge".into())
        .spawn(move || {
            let _lease = lease;
            let result = serve_bridge(listener, descriptor, reload, actions, shutdown_rx);
            if let Err(error) = result {
                eprintln!("Lapui MCP bridge stopped: {error}");
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(McpBridgeHandle {
        shutdown: Some(shutdown),
        thread: Some(thread),
    })
}

fn serve_bridge(
    listener: StdTcpListener,
    descriptor: Descriptor,
    reload: ReloadHandle,
    actions: ActionRegistry,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let listener = TcpListener::from_std(listener).map_err(|error| error.to_string())?;
        let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut shutdown = shutdown;
        loop {
            let accepted = tokio::select! {
                _ = &mut shutdown => return Ok(()),
                accepted = listener.accept() => accepted.map_err(|error| error.to_string())?,
            };
            let (stream, _) = accepted;
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let expected_token = descriptor.token.clone();
            let reload = reload.clone();
            let actions = actions.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) = serve_connection(stream, &expected_token, reload, actions).await
                {
                    eprintln!("Lapui MCP bridge client disconnected: {error}");
                }
            });
        }
    })
}

async fn serve_connection(
    mut stream: TcpStream,
    expected_token: &str,
    reload: ReloadHandle,
    actions: ActionRegistry,
) -> Result<(), String> {
    let read_line = async {
        let mut line = Vec::with_capacity(MAX_AUTH_LINE);
        loop {
            let byte = stream.read_u8().await?;
            if byte == b'\n' {
                return Ok::<_, std::io::Error>(line);
            }
            if line.len() >= MAX_AUTH_LINE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "authentication preamble exceeds the limit",
                ));
            }
            line.push(byte);
        }
    };
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), read_line)
        .await
        .map_err(|_| "MCP bridge authentication timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    let expected = format!("{MAGIC} {expected_token}");
    if !constant_time_eq(&line, expected.as_bytes()) {
        return Err("invalid MCP bridge capability".into());
    }
    let (reader, writer) = stream.into_split();
    let transport = AsyncRwTransport::<RoleServer, _, _>::new_server(reader, writer);
    LapuiMcpServer::new(reload, actions)
        .serve(transport)
        .await
        .map_err(|error| error.to_string())?
        .waiting()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// Run the stdio sidecar used by an MCP host. It forwards bytes only after
/// connecting to the authenticated, loopback-only endpoint for the given id.
pub fn run_stdio(id: &str) -> Result<(), String> {
    let path = descriptor_path(id)?;
    let descriptor: Descriptor = serde_json::from_slice(
        &fs::read(&path)
            .map_err(|error| format!("cannot read MCP bridge {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("invalid MCP bridge descriptor: {error}"))?;
    let address: std::net::SocketAddr = descriptor
        .address
        .parse()
        .map_err(|_| "MCP bridge descriptor has an invalid address")?;
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        || descriptor.token.len() != 64
        || !descriptor
            .token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("MCP bridge descriptor failed validation".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let mut stream = TcpStream::connect(address)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .write_all(format!("{MAGIC} {}\n", descriptor.token).as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        let (mut tcp_read, mut tcp_write) = stream.into_split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        tokio::select! {
            result = tokio::io::copy(&mut stdin, &mut tcp_write) => {
                tcp_write.shutdown().await.map_err(|error| error.to_string())?;
                result.map_err(|error| error.to_string())?;
            }
            result = tokio::io::copy(&mut tcp_read, &mut stdout) => {
                stdout.flush().await.map_err(|error| error.to_string())?;
                result.map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_ids_and_capability_comparison_are_bounded() {
        assert!(validate_id("files-demo_1").is_ok());
        for invalid in ["", "../escape", "has space"] {
            assert!(validate_id(invalid).is_err());
        }
        assert!(validate_id(&"x".repeat(65)).is_err());
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"short"));
    }

    #[test]
    fn dropping_bridge_handle_stops_listener_and_removes_its_descriptor() {
        use crate::reload::{DocumentSource, ReloadDocument};
        use crate::runtime::LapuiDocument;
        use std::time::{SystemTime, UNIX_EPOCH};

        let id = format!(
            "test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let actions = ActionRegistry::default();
        let (document, notify) = LapuiDocument::new(actions.clone(), None).unwrap();
        let source = DocumentSource::Embedded {
            html: "<html><body></body></html>".into(),
            script: "".into(),
        };
        let (_document, reload) =
            ReloadDocument::new(document, notify, source, actions.clone(), None);
        let path = descriptor_path(&id).unwrap();
        let bridge = start(reload, actions, &id).unwrap();
        assert!(path.exists());
        drop(bridge);
        assert!(!path.exists());
    }
}
