use std::io::{BufRead, BufReader, Write};

use std::os::unix::net::UnixStream;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::protocol::{Event, Request, Response};

use super::BoardClient;

/// The real Unix-socket client.
pub struct UnixClient {
    path: PathBuf,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl UnixClient {
    pub fn connect(path: &Path) -> anyhow::Result<UnixClient> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(UnixClient {
            path: path.to_path_buf(),
            reader,
            writer: stream,
            next_id: 0,
        })
    }

    pub fn connect_default() -> anyhow::Result<UnixClient> {
        UnixClient::connect(&crate::paths::socket_path())
    }
}

impl BoardClient for UnixClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let req = Request {
            id: id.clone(),
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;

        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                anyhow::bail!("boardd connection closed");
            }
            // Skip anything that isn't a matching response (e.g. event lines).
            let resp: Response = match serde_json::from_str(buf.trim_end()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if resp.id != id {
                continue;
            }
            if let Some(err) = resp.error {
                anyhow::bail!("boardd error {}: {}", err.code, err.message);
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        let stream = UnixStream::connect(&self.path)?;
        let mut writer = stream.try_clone()?;
        let reader = BufReader::new(stream);
        let req = Request {
            id: "sub".to_string(),
            method: "events.subscribe".to_string(),
            params: json!({}),
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        writer.flush()?;
        Ok(Box::new(EventStream { reader }))
    }
}

/// Iterator over streamed events; skips the subscribe ack and any non-event lines.
pub struct EventStream {
    reader: BufReader<UnixStream>,
}

impl Iterator for EventStream {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            let mut buf = String::new();
            match self.reader.read_line(&mut buf) {
                Ok(0) => return None,
                Ok(_) => {
                    if let Ok(ev) = serde_json::from_str::<Event>(buf.trim_end()) {
                        return Some(ev);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}
