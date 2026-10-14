//! Minimal QMP client: one connection per command, every message goes to the log.

use crate::lab::{log, Log};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub fn execute(sock: &Path, cmd: &str, args: Value, l: &Log) -> Result<Value, String> {
    let stream = UnixStream::connect(sock).map_err(|e| format!("QMP connect {}: {e}", sock.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = stream;

    read_reply(&mut reader, l)?; // greeting
    send(&mut writer, &json!({"execute": "qmp_capabilities"}), l)?;
    read_reply(&mut reader, l)?;

    let msg = if args.is_null() {
        json!({"execute": cmd})
    } else {
        json!({"execute": cmd, "arguments": args})
    };
    send(&mut writer, &msg, l)?;
    let reply = read_reply(&mut reader, l)?;
    match reply.get("error") {
        Some(e) => Err(format!("QMP {cmd}: {}", e["desc"].as_str().unwrap_or("error"))),
        None => Ok(reply["return"].clone()),
    }
}

fn send(w: &mut UnixStream, msg: &Value, l: &Log) -> Result<(), String> {
    log(l, format!("→ {msg}"));
    writeln!(w, "{msg}").map_err(|e| e.to_string())
}

/// Reads until a greeting, return or error. Events are logged and skipped.
fn read_reply(r: &mut BufReader<UnixStream>, l: &Log) -> Result<Value, String> {
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("QMP connection closed".into());
        }
        log(l, format!("← {}", line.trim_end()));
        let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if v.get("event").is_none() {
            return Ok(v);
        }
    }
}
