# qsd-lab

A small Rust GUI (egui/eframe) for experimenting with
[qemu-storage-daemon](https://www.qemu.org/docs/master/tools/qemu-storage-daemon.html)
at home. The goal is to learn QEMU's block layer (qcow2 backing chains,
snapshots, QMP, exports) outside of a VM. It may become good enough to run real
project directories through it later.

Everything runs as a normal user. Root is only needed once, for the AppArmor
setup on newer Ubuntu (see below).

## How it works

```
~/.local/share/qsd-lab/<lab>/top.qcow2   (qcow2 backing chain: base.qcow2 <- snap-*.qcow2)
  -> qemu-storage-daemon --daemonize, FUSE export on <lab>/export.raw (raw view of the disk)
  -> mkfs.ext4 the first time
  -> fuse2fs export.raw <mountpoint>      (user-chosen empty directory)
```

QMP socket: `<lab>/qmp.sock`. Every QMP message is shown in the log in the GUI.

## Features (v0.1)

- Create a lab: name, empty mountpoint directory, size in GiB.
- Start and stop a lab.
- Snapshot: live via QMP `blockdev-snapshot-sync` when running,
  `qemu-img create -b` when stopped.
- View the backing chain.
- Roll back to an older snapshot (only when stopped). This puts a new overlay
  on top of the chosen snapshot.
- Delete a lab. The mountpoint is never touched.
- Log of all commands run and all QMP JSON sent and received.

### Snapshot semantics

A snapshot freezes the current top image and puts a new, empty overlay on top
of it. Live snapshots are crash-consistent: qsd-lab runs `sync` first, but
there is no fsfreeze through fuse2fs.

## Requirements

Debian/Ubuntu package names:

- `qemu-storage-daemon`: on Ubuntu it is in `qemu-system-common`. Package names
  vary, so check with `which qemu-storage-daemon`.
- `qemu-utils` (`qemu-img`)
- `fuse2fs`
- `e2fsprogs` (`mkfs.ext4`)
- `fuse3` (`fusermount3`)
- A Rust toolchain

## Build and run

```sh
cargo run --release
```

## AppArmor (Ubuntu 24.10 and later)

On Ubuntu 24.10+, `fusermount3` is confined by AppArmor
(`/etc/apparmor.d/fusermount3`). That profile only allows FUSE mounts on
*directories*: its mount rules all end with `/`. qemu-storage-daemon's FUSE
export mounts on a regular *file* (`export.raw`), so starting a lab fails with:

```
fusermount3: failed to open .../export.raw: Permission denied
Failed to mount FUSE session to export
```

The fix is a narrow local rule,
[`packaging/apparmor/fusermount3-qsd-lab`](packaging/apparmor/fusermount3-qsd-lab),
which only allows reading, mounting and unmounting
`@{HOME}/.local/share/qsd-lab/*/export.raw`:

```
@{HOME}/.local/share/qsd-lab/*/export.raw r,
mount fstype=fuse options=(nosuid,nodev) options in (ro,rw,noatime,dirsync,nodiratime,noexec,sync) -> @{HOME}/.local/share/qsd-lab/*/export.raw,
umount @{HOME}/.local/share/qsd-lab/*/export.raw,
```

Install it:

```sh
./scripts/install-apparmor.sh
```

The script runs `sudo` itself, appends the rules inside a marked block
(`# >>> qsd-lab >>>` ... `# <<< qsd-lab <<<`) to
`/etc/apparmor.d/local/fusermount3`, and reloads the profile with
`apparmor_parser -r`. It is idempotent: running it again replaces the block.

Remove it:

```sh
./scripts/install-apparmor.sh --uninstall
```

Systems without `/etc/apparmor.d/fusermount3` do not need this. The script
detects that and exits without changing anything.

## Known limitations and ideas

- If qemu-storage-daemon crashes, `export.raw` can stay mounted. Unmount it with
  `fusermount3 -u export.raw`.
- The data path is fixed to `~/.local/share/qsd-lab`. `XDG_DATA_HOME` is not
  honored, because the AppArmor rule names the path.
- No read-only browsing of snapshots yet.
- Performance is low: data goes through userspace twice (qsd FUSE export, then
  fuse2fs). Not suited for heavy I/O.
- Overlays orphaned by a rollback are left on disk.

## Troubleshooting

Stop a lab by hand:

```sh
fusermount3 -u <mountpoint>
cd ~/.local/share/qsd-lab/<lab>
socat - UNIX-CONNECT:qmp.sock
```

Then type into the socat session:

```json
{"execute":"qmp_capabilities"}
{"execute":"quit"}
```

## License

MIT. See [LICENSE](LICENSE).
