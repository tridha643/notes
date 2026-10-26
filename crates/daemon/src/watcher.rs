//! One blocking kqueue thread (adapted from spikes/fff-check/src/vault_watcher.rs): vnode events
//! on every folder and note, then a metadata snapshot diff, then `Daemon::sync_paths` for what
//! changed. `kevent` has a NULL timeout, so nothing runs until the vault changes.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::store::{Daemon, rel_string};

/// (inode, size, mtime s, mtime ns)
type Stamp = (u64, u64, i64, i64);

#[derive(Default)]
struct Snapshot {
    notes: BTreeMap<PathBuf, Stamp>,
    /// Folder → inode, so a folder deleted and recreated at the same path gets re-armed.
    dirs: BTreeMap<PathBuf, u64>,
}

fn snapshot(dir: &Path, ino: u64, snap: &mut Snapshot) {
    snap.dirs.insert(dir.to_owned(), ino);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Skip hidden names: temp files of our atomic writes, .DS_Store, .obsidian.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let (Ok(kind), Ok(meta)) = (entry.file_type(), entry.metadata()) else {
            continue;
        };
        if kind.is_dir() {
            snapshot(&entry.path(), meta.ino(), snap);
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "md") {
            let stamp = (meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec());
            snap.notes.insert(entry.path(), stamp);
        }
    }
}

fn take_snapshot(vault: &Path) -> Snapshot {
    let mut snap = Snapshot::default();
    snapshot(vault, fs::metadata(vault).map_or(0, |m| m.ino()), &mut snap);
    snap
}

fn arm(queue: &OwnedFd, path: &Path, handles: &mut BTreeMap<PathBuf, File>) {
    handles.remove(path);
    // O_EVTONLY: watch without keeping the volume busy. Best effort: the path may be gone already.
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_EVTONLY)
        .open(path)
    else {
        return;
    };
    let event = libc::kevent {
        ident: file.as_raw_fd() as usize,
        filter: libc::EVFILT_VNODE,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: libc::NOTE_WRITE
            | libc::NOTE_DELETE
            | libc::NOTE_RENAME
            | libc::NOTE_ATTRIB
            | libc::NOTE_EXTEND,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let added = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if added == 0 {
        handles.insert(path.to_owned(), file);
    }
}

/// Arm new notes/folders (and new inodes after atomic replaces), drop vanished ones, and
/// return the notes whose stamp changed. Returns whether anything was newly armed.
fn diff(
    queue: &OwnedFd,
    vault: &Path,
    previous: &Snapshot,
    current: &Snapshot,
    handles: &mut BTreeMap<PathBuf, File>,
    changed: &mut Vec<String>,
) -> bool {
    let mut armed = false;
    for (path, stamp) in &current.notes {
        let before = previous.notes.get(path);
        if before != Some(stamp) {
            changed.push(rel_string(vault, path));
        }
        if before.map(|s| s.0) != Some(stamp.0) {
            arm(queue, path, handles);
            armed = true;
        }
    }
    for path in previous
        .notes
        .keys()
        .filter(|p| !current.notes.contains_key(*p))
    {
        changed.push(rel_string(vault, path));
    }
    for (dir, ino) in &current.dirs {
        if previous.dirs.get(dir) != Some(ino) {
            arm(queue, dir, handles);
            armed = true;
        }
    }
    handles.retain(|path, _| current.notes.contains_key(path) || current.dirs.contains_key(path));
    armed
}

pub fn start(daemon: Arc<Daemon>) -> io::Result<()> {
    // ponytail: one fd per note and folder; fine to ~20k notes, FSEvents if vaults get bigger.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur = limit.rlim_max.min(20_000).max(limit.rlim_cur);
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
    }
    let fd = unsafe { libc::kqueue() };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let queue = unsafe { OwnedFd::from_raw_fd(fd) };
    let vault = daemon.vault.clone();
    let mut handles = BTreeMap::new();
    // Startup races are covered by `Daemon::reconcile`, which runs after this returns.
    let mut previous = take_snapshot(&vault);
    diff(
        &queue,
        &vault,
        &Snapshot::default(),
        &previous,
        &mut handles,
        &mut Vec::new(),
    );
    std::thread::Builder::new()
        .name("notes-vault-kqueue".into())
        .spawn(move || {
            let mut received = [libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            }; 128];
            loop {
                // NULL timeout: blocks in the kernel until a vnode event.
                let n = unsafe {
                    libc::kevent(
                        queue.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        received.as_mut_ptr(),
                        received.len() as i32,
                        std::ptr::null(),
                    )
                };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    eprintln!("notesd: vault watcher stopped: {error}");
                    return;
                }
                // A write that lands after a snapshot but before its vnode is armed raises no
                // event, so snapshot again after arming anything new, until nothing new is armed.
                loop {
                    let current = take_snapshot(&vault);
                    let mut changed = Vec::new();
                    let armed = diff(
                        &queue,
                        &vault,
                        &previous,
                        &current,
                        &mut handles,
                        &mut changed,
                    );
                    previous = current;
                    if !changed.is_empty() {
                        daemon.sync_paths(&changed);
                    }
                    if !armed {
                        break;
                    }
                }
            }
        })?;
    Ok(())
}

/// Watch the sideshow session folder (flat: session files plus `assets/`). Any change to
/// its entries (a file added, removed, or atomically replaced) runs `Daemon::stage_refresh`,
/// which ignores notesd's own writes.
pub fn start_stage(daemon: Arc<Daemon>) -> io::Result<()> {
    let fd = unsafe { libc::kqueue() };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let queue = unsafe { OwnedFd::from_raw_fd(fd) };
    let root = daemon.stage_path();
    let mut handles = BTreeMap::new();
    arm(&queue, &root, &mut handles);
    if handles.is_empty() {
        return Err(io::Error::other(format!("can't watch {}", root.display())));
    }
    std::thread::Builder::new()
        .name("notes-stage-kqueue".into())
        .spawn(move || {
            let mut received = [libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            }; 16];
            loop {
                // NULL timeout: blocks in the kernel until the folder changes.
                let n = unsafe {
                    libc::kevent(
                        queue.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        received.as_mut_ptr(),
                        received.len() as i32,
                        std::ptr::null(),
                    )
                };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    eprintln!("notesd: session folder watcher stopped: {error}");
                    return;
                }
                if received[..n as usize]
                    .iter()
                    .any(|e| e.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0)
                {
                    // The folder itself went away or moved: re-arm on whatever is there now.
                    let _ = fs::create_dir_all(&root);
                    arm(&queue, &root, &mut handles);
                }
                daemon.stage_refresh();
            }
        })?;
    Ok(())
}
