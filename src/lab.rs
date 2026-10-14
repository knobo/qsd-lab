//! A lab is one qcow2 backing chain served by qemu-storage-daemon:
//!
//!   top.qcow2 -> qsd (FUSE export on export.raw) -> fuse2fs -> mountpoint
//!
//! Everything about a lab lives in `~/.local/share/qsd-lab/<name>/`.
//! `lab.json` there is the only source of truth; the UI reloads it after every action.

use crate::qmp;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Shared, append-only log of commands, QMP traffic and errors shown in the UI.
pub type Log = Arc<Mutex<Vec<String>>>;

pub fn log(l: &Log, s: impl Into<String>) {
    l.lock().unwrap().push(s.into());
}

/// `~/.local/share/qsd-lab`. Fixed path because the AppArmor rule names it.
pub fn data_root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/qsd-lab")
}

/// Missing tools or setup, as human-readable lines. Empty = ready.
pub fn prereq_problems() -> Vec<String> {
    let mut problems: Vec<String> = ["qemu-storage-daemon", "qemu-img", "mkfs.ext4", "fuse2fs", "fusermount3"]
        .iter()
        .filter(|tool| !on_path(tool))
        .map(|tool| format!("{tool} not found on PATH"))
        .collect();
    let confined = Path::new("/etc/apparmor.d/fusermount3").exists();
    let local = fs::read_to_string("/etc/apparmor.d/local/fusermount3").unwrap_or_default();
    if confined && !local.contains("qsd-lab") {
        problems.push(
            "fusermount3 is confined by AppArmor and the qsd-lab rule is missing: run scripts/install-apparmor.sh"
                .into(),
        );
    }
    problems
}

fn on_path(tool: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| Path::new(dir).join(tool).exists())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lab {
    pub name: String,
    pub mountpoint: PathBuf,
    pub size_gib: u64,
    /// File name (in the lab dir) of the active, writable image.
    pub top: String,
    /// Set once mkfs.ext4 has run on the export.
    pub formatted: bool,
}

/// One image in the backing chain, newest (active) first.
#[derive(Clone, Debug)]
pub struct Image {
    pub file: String,
    /// True for the writable top image; all others are frozen snapshots.
    pub active: bool,
    /// Seconds since the file was last written (for a snapshot: since it was frozen).
    pub age_secs: u64,
}

impl Lab {
    pub fn dir(&self) -> PathBuf {
        data_root().join(&self.name)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir().join(name)
    }

    fn export(&self) -> PathBuf {
        self.file("export.raw")
    }

    fn sock(&self) -> PathBuf {
        self.file("qmp.sock")
    }

    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).unwrap();
        fs::write(self.file("lab.json"), json).map_err(|e| format!("write lab.json: {e}"))
    }

    pub fn load_all() -> Vec<Lab> {
        let mut labs: Vec<Lab> = fs::read_dir(data_root())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path().join("lab.json")).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        labs.sort_by(|a, b| a.name.cmp(&b.name));
        labs
    }

    /// Creates the lab dir, base.qcow2 and lab.json. Mountpoint must be an existing empty directory.
    pub fn create(name: &str, mountpoint: &Path, size_gib: u64, log: &Log) -> Result<Lab, String> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("name must be letters, digits, '-' or '_'".into());
        }
        let mountpoint = mountpoint.canonicalize().map_err(|e| format!("{}: {e}", mountpoint.display()))?;
        check_empty_dir(&mountpoint)?;
        if mountpoint.starts_with(data_root()) {
            return Err("mountpoint can not be inside the qsd-lab data dir".into());
        }
        let lab = Lab { name: name.into(), mountpoint, size_gib, top: "base.qcow2".into(), formatted: false };
        if lab.dir().exists() {
            return Err(format!("lab {name} already exists"));
        }
        fs::create_dir_all(lab.dir()).map_err(|e| e.to_string())?;
        run(
            Command::new("qemu-img")
                .args(["create", "-f", "qcow2"])
                .arg(lab.file("base.qcow2"))
                .arg(format!("{size_gib}G")),
            log,
        )?;
        lab.save()?;
        Ok(lab)
    }

    pub fn qsd_running(&self) -> bool {
        fs::read_to_string(self.file("qsd.pid"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
    }

    pub fn mounted(&self) -> bool {
        is_mountpoint(&self.mountpoint)
    }

    pub fn start(&mut self, log: &Log) -> Result<(), String> {
        if !self.qsd_running() {
            if is_mountpoint(&self.export()) {
                // Left behind by a crashed qsd.
                run(Command::new("fusermount3").arg("-u").arg(self.export()), log)?;
            }
            fs::write(self.export(), b"").map_err(|e| e.to_string())?;
            let _ = fs::remove_file(self.sock());
            run(
                Command::new("qemu-storage-daemon")
                    .args(["--daemonize", "--pidfile"])
                    .arg(self.file("qsd.pid"))
                    .arg("--blockdev")
                    .arg(format!("driver=file,node-name=file0,filename={}", self.file(&self.top).display()))
                    .args(["--blockdev", "driver=qcow2,node-name=disk,file=file0"])
                    .arg("--export")
                    .arg(format!(
                        "type=fuse,id=exp0,node-name=disk,mountpoint={},writable=on,allow-other=off",
                        self.export().display()
                    ))
                    .arg("--chardev")
                    .arg(format!("socket,id=qmp,path={},server=on,wait=off", self.sock().display()))
                    .args(["--monitor", "chardev=qmp"]),
                log,
            )?;
        }
        if !self.formatted {
            let meta = fs::metadata(self.dir()).map_err(|e| e.to_string())?;
            run(
                Command::new("mkfs.ext4")
                    .args(["-F", "-q", "-E"])
                    .arg(format!("root_owner={}:{}", meta.uid(), meta.gid()))
                    .arg(self.export()),
                log,
            )?;
            self.formatted = true;
            self.save()?;
        }
        if !self.mounted() {
            check_empty_dir(&self.mountpoint)?;
            run(Command::new("fuse2fs").arg(self.export()).arg(&self.mountpoint), log)?;
        }
        Ok(())
    }

    pub fn stop(&self, log: &Log) -> Result<(), String> {
        if self.mounted() {
            run(Command::new("fusermount3").arg("-u").arg(&self.mountpoint), log)?;
        }
        // fuse2fs flushes the ext4 metadata as it exits; qsd must outlive it.
        let export = self.export().display().to_string();
        wait_until(log, "fuse2fs to exit", || !fuse2fs_running(&export))?;
        if self.qsd_running() {
            qmp::execute(&self.sock(), "quit", json!(null), log)?;
            wait_until(log, "qemu-storage-daemon to exit", || !self.qsd_running())?;
        }
        Ok(())
    }

    /// Live snapshot if running, offline otherwise. The old top becomes a frozen snapshot.
    pub fn snapshot(&mut self, log: &Log) -> Result<(), String> {
        let new = self.new_name()?;
        if self.qsd_running() {
            // ponytail: crash-consistent only; fuse2fs has no fsfreeze. Unmount first for a clean snapshot.
            if self.mounted() {
                run(Command::new("sync").arg("-f").arg(&self.mountpoint), log)?;
            }
            // The export follows the top of the graph, so ask it which node that is.
            let exports = qmp::execute(&self.sock(), "query-block-exports", json!(null), log)?;
            let node = exports[0]["node-name"].as_str().ok_or("no block export found")?.to_string();
            qmp::execute(
                &self.sock(),
                "blockdev-snapshot-sync",
                json!({
                    "node-name": node,
                    "snapshot-file": self.file(&new),
                    "snapshot-node-name": new.trim_end_matches(".qcow2"),
                    "format": "qcow2",
                }),
                log,
            )?;
        } else {
            self.new_overlay(&self.top.clone(), &new, log)?;
        }
        self.top = new;
        self.save()
    }

    /// Lab must be stopped. New empty top on top of `file` (a frozen image in the chain).
    pub fn rollback(&mut self, file: &str, log: &Log) -> Result<(), String> {
        if self.qsd_running() || self.mounted() {
            return Err("stop the lab before rollback".into());
        }
        let new = self.new_name()?;
        self.new_overlay(file, &new, log)?;
        // ponytail: the old branch stays on disk; add cleanup of unreferenced images if it piles up.
        self.top = new;
        self.save()
    }

    /// Unique image name; qemu-img create would silently overwrite an existing file.
    fn new_name(&self) -> Result<String, String> {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
        let name = format!("snap-{ms}.qcow2");
        match self.file(&name).exists() {
            true => Err(format!("{name} already exists")),
            false => Ok(name),
        }
    }

    fn new_overlay(&self, backing: &str, new: &str, log: &Log) -> Result<(), String> {
        run(
            Command::new("qemu-img")
                .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
                .arg(self.file(backing))
                .arg(self.file(new)),
            log,
        )
        .map(|_| ())
    }

    pub fn chain(&self, log: &Log) -> Result<Vec<Image>, String> {
        let out = run(
            Command::new("qemu-img")
                .args(["info", "-U", "--backing-chain", "--output=json"])
                .arg(self.file(&self.top)),
            log,
        )?;
        let infos: Vec<serde_json::Value> = serde_json::from_str(&out).map_err(|e| e.to_string())?;
        Ok(infos
            .iter()
            .enumerate()
            .filter_map(|(i, info)| {
                let path = PathBuf::from(info["filename"].as_str()?);
                let age_secs = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map_or(0, |d| d.as_secs());
                Some(Image { file: path.file_name()?.to_string_lossy().into(), active: i == 0, age_secs })
            })
            .collect())
    }

    /// Lab must be stopped. Removes the lab dir; never touches the mountpoint.
    pub fn delete(&self, l: &Log) -> Result<(), String> {
        if self.qsd_running() || self.mounted() {
            return Err("stop the lab before deleting it".into());
        }
        log(l, format!("$ rm -r {}", self.dir().display()));
        fs::remove_dir_all(self.dir()).map_err(|e| e.to_string())
    }
}

/// Runs a command, logs it and its output, returns stdout.
fn run(cmd: &mut Command, l: &Log) -> Result<String, String> {
    let shown = format!(
        "$ {} {}",
        cmd.get_program().to_string_lossy(),
        cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
    );
    log(l, shown);
    let out = cmd.output().map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // qemu-img info JSON is long; the chain table shows it anyway.
    if !stdout.trim().is_empty() && stdout.len() < 2000 {
        log(l, stdout.trim_end().to_string());
    }
    if !stderr.trim().is_empty() {
        log(l, stderr.trim_end().to_string());
    }
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{} failed ({})", cmd.get_program().to_string_lossy(), out.status))
    }
}

fn check_empty_dir(p: &Path) -> Result<(), String> {
    let mut entries = fs::read_dir(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if entries.next().is_some() {
        return Err(format!("{} is not empty", p.display()));
    }
    Ok(())
}

fn is_mountpoint(p: &Path) -> bool {
    // ponytail: paths with spaces are escaped (\040) in mountinfo and will not match.
    let p = p.to_string_lossy();
    fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_default()
        .lines()
        .any(|l| l.split(' ').nth(4) == Some(&*p))
}

fn fuse2fs_running(export: &str) -> bool {
    fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        let cmdline = fs::read(e.path().join("cmdline")).unwrap_or_default();
        let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).collect();
        args.first().is_some_and(|a| a.ends_with(b"fuse2fs")) && args.contains(&export.as_bytes())
    })
}

fn wait_until(l: &Log, what: &str, done: impl Fn() -> bool) -> Result<(), String> {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > Duration::from_secs(30) {
            return Err(format!("timed out waiting for {what}"));
        }
        sleep(Duration::from_millis(100));
    }
    log(l, format!("ok: {what} ({} ms)", start.elapsed().as_millis()));
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    /// offline_chain changes HOME, which data_root() reads; tests must not overlap.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Offline path only (no FUSE): create, snapshot, rollback, delete.
    #[test]
    fn offline_chain() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let real_home = std::env::var("HOME").unwrap();
        let home = std::env::temp_dir().join(format!("qsd-lab-test-{}", std::process::id()));
        let mnt = home.join("mnt");
        fs::create_dir_all(&mnt).unwrap();
        unsafe { std::env::set_var("HOME", &home) };
        let l = Log::default();

        let mut lab = Lab::create("t", &mnt, 1, &l).unwrap();
        lab.snapshot(&l).unwrap();
        let chain = lab.chain(&l).unwrap();
        assert_eq!(chain.len(), 2);
        assert!(chain[0].active && chain[1].file == "base.qcow2");

        lab.rollback("base.qcow2", &l).unwrap();
        let chain = lab.chain(&l).unwrap();
        assert_eq!(chain.iter().map(|i| i.file.as_str()).last(), Some("base.qcow2"));
        assert_eq!(chain.len(), 2);

        lab.delete(&l).unwrap();
        assert!(Lab::load_all().is_empty());
        unsafe { std::env::set_var("HOME", real_home) };
        fs::remove_dir_all(&home).unwrap();
    }

    /// Full chain with FUSE. Needs the AppArmor rule; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_chain() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mnt = std::env::temp_dir().join(format!("qsd-lab-mnt-{}", std::process::id()));
        fs::create_dir_all(&mnt).unwrap();
        let l = Log::default();
        let name = format!("selftest-{}", std::process::id());
        let mut lab = Lab::create(&name, &mnt, 1, &l).unwrap();
        let res = (|| {
            lab.start(&l)?;
            assert!(lab.qsd_running() && lab.mounted());
            fs::write(mnt.join("a.txt"), "one").map_err(|e| e.to_string())?;
            lab.snapshot(&l)?;
            fs::write(mnt.join("a.txt"), "two").map_err(|e| e.to_string())?;
            lab.stop(&l)?;
            assert!(!lab.qsd_running() && !lab.mounted());

            let frozen = lab.chain(&l)?[1].file.clone();
            lab.rollback(&frozen, &l)?;
            lab.start(&l)?;
            let text = fs::read_to_string(mnt.join("a.txt")).map_err(|e| e.to_string())?;
            lab.stop(&l)?;
            Ok::<_, String>(text)
        })();
        let _ = lab.stop(&l);
        for line in l.lock().unwrap().iter() {
            println!("{line}");
        }
        lab.delete(&l).unwrap();
        fs::remove_dir(&mnt).unwrap();
        assert_eq!(res.unwrap(), "one");
    }
}
