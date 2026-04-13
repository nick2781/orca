use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info};

use crate::protocol::{RpcRequest, RpcResponse, RpcError, PARSE_ERROR};

/// Handler function type for processing JSON-RPC requests.
pub type RpcHandler = Arc<dyn Fn(RpcRequest) -> RpcResponse + Send + Sync>;

/// A JSON-RPC server over Unix domain sockets using newline-delimited JSON.
pub struct IpcServer {
    listener: UnixListener,
    handler: RpcHandler,
}

impl IpcServer {
    /// Bind to a Unix socket at the given path.
    ///
    /// Removes any stale socket file, creates parent directories if needed,
    /// and binds a `UnixListener`.
    pub fn bind(socket_path: &Path, handler: RpcHandler) -> Result<Self> {
        // Create parent directories if they don't exist
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dirs for {}", socket_path.display()))?;
        }

        // Remove stale socket file
        if socket_path.exists() {
            std::fs::remove_file(socket_path)
                .with_context(|| format!("failed to remove stale socket {}", socket_path.display()))?;
        }

        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("failed to bind Unix socket at {}", socket_path.display()))?;

        info!("IPC server bound to {}", socket_path.display());

        Ok(Self { listener, handler })
    }

    /// Run the accept loop, spawning a task for each incoming connection.
    pub async fn run(self) -> Result<()> {
        loop {
            let (stream, _addr) = self.listener.accept().await
                .context("failed to accept connection")?;

            let handler = Arc::clone(&self.handler);
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, handler).await {
                    error!("connection error: {e:#}");
                }
            });
        }
    }
}

/// Handle a single client connection: read lines, parse as RpcRequest, call handler, write response.
async fn handle_connection(stream: UnixStream, handler: RpcHandler) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await
            .context("failed to read line from client")?;

        // EOF — client disconnected
        if n == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RpcRequest>(trimmed) {
            Ok(request) => handler(request),
            Err(e) => RpcResponse::error(
                serde_json::Value::Null,
                RpcError {
                    code: PARSE_ERROR,
                    message: format!("parse error: {e}"),
                    data: None,
                },
            ),
        };

        let mut resp_bytes = serde_json::to_vec(&response)
            .context("failed to serialize response")?;
        resp_bytes.push(b'\n');

        writer.write_all(&resp_bytes).await
            .context("failed to write response")?;
        writer.flush().await
            .context("failed to flush response")?;
    }

    Ok(())
}

/// A JSON-RPC client over Unix domain sockets.
pub struct IpcClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl IpcClient {
    /// Connect to a Unix socket at the given path.
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await
            .with_context(|| format!("failed to connect to {}", socket_path.display()))?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        })
    }

    /// Send a JSON-RPC request and read the response.
    pub async fn call(&mut self, request: &RpcRequest) -> Result<RpcResponse> {
        let mut req_bytes = serde_json::to_vec(request)
            .context("failed to serialize request")?;
        req_bytes.push(b'\n');

        self.writer.write_all(&req_bytes).await
            .context("failed to write request")?;
        self.writer.flush().await
            .context("failed to flush request")?;

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await
            .context("failed to read response")?;

        if n == 0 {
            anyhow::bail!("server closed connection before responding");
        }

        let response: RpcResponse = serde_json::from_str(line.trim())
            .context("failed to parse response")?;

        Ok(response)
    }
}
