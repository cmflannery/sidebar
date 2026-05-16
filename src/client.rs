//! CLI-side unix socket client. Connects, says Hello as the given identity,
//! sends one or more requests, parses responses.

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::paths;
use crate::proto::{Hello, Op, Request, Response};

pub struct Client {
    reader: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Client {
    /// Connect to the daemon and send the Hello frame.
    pub async fn connect_as(speaking_as: &str) -> Result<Self> {
        let path = paths::socket()?;
        let stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("connecting to daemon at {}", path.display()))?;
        let (read, mut write) = stream.into_split();
        let mut hello = serde_json::to_vec(&Hello::Cli {
            speaking_as: speaking_as.to_string(),
        })?;
        hello.push(b'\n');
        write.write_all(&hello).await?;
        Ok(Self {
            reader: BufReader::new(read).lines(),
            writer: write,
            next_id: 1,
        })
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
