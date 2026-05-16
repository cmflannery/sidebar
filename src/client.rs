//! CLI-side unix socket client. Connects, says Hello as the given identity,
//! reads the daemon's HelloAck, then sends/receives Request/Response pairs.

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::paths;
use crate::proto::{Hello, HelloAck, Op, Request, Response};

pub struct Client {
    reader: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    next_id: u64,
    /// Name the daemon actually assigned. For MCP clients this may differ
    /// from the name passed in if another session held it.
    assigned_name: String,
}

impl Client {
    /// Connect to the daemon as a CLI client speaking as `speaking_as`.
    pub async fn connect_as(speaking_as: &str) -> Result<Self> {
        Self::connect_with_hello(Hello::Cli {
            speaking_as: speaking_as.to_string(),
        })
        .await
    }

    /// Connect to the daemon as an MCP stub for `agent`. The returned
    /// client's `assigned_name()` may be a suffixed version of `agent`.
    pub async fn connect_mcp(agent: &str, version: &str) -> Result<Self> {
        Self::connect_with_hello(Hello::Mcp {
            agent: agent.to_string(),
            version: version.to_string(),
        })
        .await
    }

    async fn connect_with_hello(hello: Hello) -> Result<Self> {
        let path = paths::socket()?;
        let stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("connecting to daemon at {}", path.display()))?;
        let (read, mut write) = stream.into_split();
        let mut bytes = serde_json::to_vec(&hello)?;
        bytes.push(b'\n');
        write.write_all(&bytes).await?;

        let mut reader = BufReader::new(read).lines();
        let ack_line = reader
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon closed connection before HelloAck"))?;
        let ack: HelloAck = serde_json::from_str(&ack_line).context("parsing HelloAck frame")?;

        Ok(Self {
            reader,
            writer: write,
            next_id: 1,
            assigned_name: ack.agent,
        })
    }

    pub fn assigned_name(&self) -> &str {
        &self.assigned_name
    }

    pub async fn request(&mut self, op: Op) -> Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request { id, op };
        let mut bytes = serde_json::to_vec(&req)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        let line = self
            .reader
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon closed connection without responding"))?;
        let resp: Response = serde_json::from_str(&line).context("parsing response")?;
        if resp.id != id {
            anyhow::bail!("response id mismatch: expected {id}, got {}", resp.id);
        }
        Ok(resp)
    }
}
