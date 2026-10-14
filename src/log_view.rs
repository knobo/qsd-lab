//! Renders log lines with colors and short explanations of what each step does.
//! Lines look like `YYYY-MM-DD HH:MM:SS <text>`; see `lab::Log`.

use eframe::egui::{self, Color32, RichText};
use serde_json::Value;

const CMD: Color32 = Color32::from_rgb(90, 160, 255);
const QMP_OUT: Color32 = Color32::from_rgb(190, 130, 255);
const EVENT: Color32 = Color32::from_rgb(255, 170, 60);
const OK: Color32 = Color32::from_rgb(80, 200, 120);
const ERR: Color32 = Color32::from_rgb(255, 90, 90);

/// `opts` = (explain, raw_json, handshake).
pub fn show(ui: &mut egui::Ui, lines: &[String], (explain, raw_json, handshake): (bool, bool, bool)) {
    let mut hide_next_reply = false;
    for line in lines {
        let (ts, text) = split_ts(line);
        if !handshake {
            if is_handshake(text, &mut hide_next_reply) {
                continue;
            }
        }
        let (shown, color, note) = describe(text);
        if text.starts_with("== ") {
            ui.add_space(6.0);
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(time_of(ts)).monospace().weak().small());
            let mut rt = RichText::new(shown).monospace();
            if let Some(c) = color {
                rt = rt.color(c);
            }
            if text.starts_with("== ") {
                rt = rt.strong();
            }
            ui.label(rt);
        });
        if raw_json && (text.starts_with("→ ") || text.starts_with("← ")) {
            ui.label(RichText::new(format!("      {}", &text[4..])).monospace().weak().small());
        }
        if explain && let Some(note) = note {
            ui.label(RichText::new(format!("      {note}")).italics().weak());
        }
    }
}

fn split_ts(line: &str) -> (&str, &str) {
    // 19 chars of "YYYY-MM-DD HH:MM:SS" then a space.
    match line.get(..19) {
        Some(ts) if line.as_bytes().get(19) == Some(&b' ') => (ts, &line[20..]),
        _ => ("", line),
    }
}

fn time_of(ts: &str) -> &str {
    ts.get(11..).unwrap_or(ts)
}

/// Greeting, qmp_capabilities and its empty reply: noise on every QMP call.
fn is_handshake(text: &str, hide_next_reply: &mut bool) -> bool {
    let Some(json) = qmp_json(text) else { return false };
    if json.get("QMP").is_some() {
        return true;
    }
    if json["execute"] == "qmp_capabilities" {
        *hide_next_reply = true;
        return true;
    }
    if text.starts_with("← ") && json.get("return").is_some() && std::mem::take(hide_next_reply) {
        return true;
    }
    false
}

fn qmp_json(text: &str) -> Option<Value> {
    let body = text.strip_prefix("→ ").or_else(|| text.strip_prefix("← "))?;
    serde_json::from_str(body).ok()
}

/// What to show, in which color, and an optional explanation.
fn describe(text: &str) -> (String, Option<Color32>, Option<&'static str>) {
    if let Some(cmd) = text.strip_prefix("$ ") {
        return (text.into(), Some(CMD), explain_cmd(cmd));
    }
    if text.starts_with("ok: ") {
        return (text.into(), Some(OK), None);
    }
    if text.starts_with("ERROR") {
        return (text.into(), Some(ERR), None);
    }
    if text.starts_with("== ") {
        return (text.into(), None, None);
    }
    let Some(json) = qmp_json(text) else {
        return (text.into(), Some(Color32::GRAY), None);
    };
    if let Some(cmd) = json["execute"].as_str() {
        return (format!("QMP → {cmd}"), Some(QMP_OUT), explain_qmp(cmd));
    }
    if let Some(event) = json["event"].as_str() {
        return (format!("QMP ← event {event}"), Some(EVENT), explain_event(event));
    }
    if let Some(e) = json.get("error") {
        return (format!("QMP ← error: {}", e["desc"].as_str().unwrap_or("?")), Some(ERR), None);
    }
    if json.get("QMP").is_some() {
        return ("QMP ← greeting".into(), Some(Color32::GRAY), Some("qsd says hello and lists its version."));
    }
    let ret = &json["return"];
    let empty = ret.as_object().is_some_and(|o| o.is_empty());
    let shown = if empty { "QMP ← ok".to_string() } else { format!("QMP ← return {ret}") };
    (shown, Some(Color32::GRAY), None)
}

fn explain_cmd(cmd: &str) -> Option<&'static str> {
    let mut words = cmd.split_whitespace();
    let prog = words.next()?.rsplit('/').next()?;
    let rest: Vec<&str> = words.collect();
    Some(match prog {
        "qemu-img" if rest.first() == Some(&"info") => {
            "Read the image headers and follow the backing chain (-U: allowed while qsd has the image open)."
        }
        "qemu-img" if rest.contains(&"-b") => {
            "Create an empty qcow2 overlay on top of a backing file. Reads fall through to the backing file; writes stay in the overlay."
        }
        "qemu-img" => "Create a new, empty qcow2 image. Space is only allocated as data is written.",
        "qemu-storage-daemon" => {
            "Start QEMU's block layer without a VM: open the image as a block node, export it as a raw file over FUSE (export.raw) and listen for QMP on qmp.sock."
        }
        "mkfs.ext4" => {
            "Format the exported disk as ext4. Every write goes through FUSE into qsd, which stores it in the qcow2 image."
        }
        "fuse2fs" => "Mount the ext4 file system in export.raw on your directory, in userspace (no root needed).",
        "fusermount3" => "Unmount a FUSE file system.",
        "sync" => "Flush cached writes of the mounted file system before taking the snapshot.",
        "rm" => "Delete the lab directory and all its images. The mountpoint is not touched.",
        _ => return None,
    })
}

fn explain_qmp(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "qmp_capabilities" => "Handshake: leave negotiation mode so qsd accepts commands.",
        "query-block-exports" => "Ask which block node the FUSE export serves right now (the top of the chain).",
        "blockdev-snapshot-sync" => {
            "Live snapshot: create a new overlay, make the current top its backing file (read-only from now on, so it is frozen) and switch the export to the overlay without stopping."
        }
        "quit" => "Stop qemu-storage-daemon. The FUSE export on export.raw is unmounted as it exits.",
        _ => return None,
    })
}

fn explain_event(event: &str) -> Option<&'static str> {
    Some(match event {
        "SHUTDOWN" => "qsd is shutting down.",
        "BLOCK_EXPORT_DELETED" => "The FUSE export is gone; export.raw is a plain empty file again.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_handshake_but_not_real_reply() {
        let lines = [
            r#"← {"QMP": {"version": {}}}"#,
            r#"→ {"execute":"qmp_capabilities"}"#,
            r#"← {"return": {}}"#,
            r#"→ {"execute":"quit"}"#,
            r#"← {"return": {}}"#,
        ];
        let mut hide = false;
        let kept: Vec<_> = lines.iter().filter(|l| !is_handshake(l, &mut hide)).collect();
        assert_eq!(kept, [&lines[3], &lines[4]]);
    }

    #[test]
    fn splits_timestamp() {
        assert_eq!(split_ts("2026-10-14 15:22:01 $ sync"), ("2026-10-14 15:22:01", "$ sync"));
        assert_eq!(split_ts("no stamp"), ("", "no stamp"));
        assert_eq!(describe(r#"→ {"execute":"quit"}"#).0, "QMP → quit");
    }
}
