//! Spike-only vnode watcher: reconcile metadata after events, never on a timer.
use fff_search::SharedFilePicker;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

type FileStamp = (u64, u64, i64, i64);
type VaultSnapshot = BTreeMap<PathBuf, FileStamp>;

/// Feed known writes and watcher changes through the same public fff update calls.
pub fn apply_vault_update(shared: &SharedFilePicker, path: &Path) -> Result<&'static str, String> {
    let mut guard = shared.write().map_err(|e| e.to_string())?;
    let picker = guard.as_mut().ok_or("fff-check: missing update picker")?;
    if path.exists() {
        picker
            .handle_create_or_modify(path)
            .ok_or("fff-check: manual update rejected")?;
        Ok("created_or_modified")
    } else {
        picker.remove_file_by_path(path);
        Ok("removed")
    }
}

fn snapshot_vault(
    path: &Path,
    files: &mut VaultSnapshot,
    directories: &mut Vec<PathBuf>,
) -> io::Result<()> {
    directories.push(path.to_owned());
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            snapshot_vault(&entry.path(), files, directories)?;
        } else if entry.path().extension().is_some_and(|ext| ext == "md") {
            files.insert(
                entry.path(),
                (meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec()),
            );
        }
    }
    Ok(())
}

fn queue_event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn register_vnode(
    queue: &OwnedFd,
    path: &Path,
    handles: &mut BTreeMap<PathBuf, File>,
) -> io::Result<()> {
    let file = File::open(path)?;
    let event = queue_event(
        file.as_raw_fd() as usize,
        libc::EVFILT_VNODE,
        libc::EV_ADD | libc::EV_CLEAR,
        libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_ATTRIB,
    );
    if unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    handles.insert(path.to_owned(), file);
    Ok(())
}

/// One blocking kqueue thread, explicitly joined before the fff-only idle control.
pub struct VaultWatcher {
    queue: Arc<OwnedFd>,
    thread: JoinHandle<Result<(), String>>,
    events: Arc<Mutex<Vec<Value>>>,
}

impl VaultWatcher {
    /// Register existing directories and notes, including in-place writes and atomic saves.
    pub fn start(
        vault: &Path,
        shared: SharedFilePicker,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Vnode watching needs an fd per note; this is an intentionally bounded spike.
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        limit.rlim_cur = limit.rlim_max.min(20_000);
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let queue = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        let stop = queue_event(1, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0);
        if unsafe { libc::kevent(fd, &stop, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut previous = VaultSnapshot::new();
        let mut directories = Vec::new();
        snapshot_vault(vault, &mut previous, &mut directories)?;
        let mut handles = BTreeMap::new();
        for path in directories.iter().chain(previous.keys()) {
            register_vnode(&queue, path, &mut handles)?;
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let pending = events.clone();
        let thread_queue = queue.clone();
        let vault = vault.to_owned();
        let thread = std::thread::Builder::new()
            .name("notes-vault-kqueue".into())
            .spawn(move || {
                loop {
                    let mut received = [queue_event(0, 0, 0, 0); 128];
                    // NULL timeout: the kernel blocks until a vnode event or explicit shutdown.
                    let count = unsafe {
                        libc::kevent(
                            thread_queue.as_raw_fd(),
                            std::ptr::null(),
                            0,
                            received.as_mut_ptr(),
                            received.len() as i32,
                            std::ptr::null(),
                        )
                    };
                    if count < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        return Err(error.to_string());
                    }
                    if received[..count as usize]
                        .iter()
                        .any(|event| event.filter == libc::EVFILT_USER)
                    {
                        return Ok(());
                    }
                    let mut current = VaultSnapshot::new();
                    let mut directories = Vec::new();
                    snapshot_vault(&vault, &mut current, &mut directories)
                        .map_err(|e| e.to_string())?;
                    let mut changed = Vec::new();
                    for (path, stamp) in &current {
                        if previous.get(path) != Some(stamp) {
                            let action = apply_vault_update(&shared, path)?;
                            changed.push(json!({"path": path, "kind": action, "source": "kqueue"}));
                            // Re-arm on the new inode after atomic replacement.
                            register_vnode(&thread_queue, path, &mut handles)
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    for path in previous.keys().filter(|path| !current.contains_key(*path)) {
                        let action = apply_vault_update(&shared, path)?;
                        changed.push(json!({"path": path, "kind": action, "source": "kqueue"}));
                        handles.remove(path);
                    }
                    for path in directories
                        .iter()
                        .filter(|path| !handles.contains_key(*path))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        register_vnode(&thread_queue, &path, &mut handles)
                            .map_err(|e| e.to_string())?;
                    }
                    previous = current;
                    pending.lock().unwrap().extend(changed);
                }
            })?;
        Ok(Self {
            queue,
            thread,
            events,
        })
    }

    /// Drain acknowledged watcher updates only on an explicit driver request.
    pub fn drain_events(&self) -> Vec<Value> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// Wake via EVFILT_USER and join; no poll interval is needed for shutdown.
    pub fn stop(self) -> Result<(), Box<dyn std::error::Error>> {
        let event = queue_event(1, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        if unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
        self.thread
            .join()
            .map_err(|_| "fff-check: kqueue thread panicked")??;
        Ok(())
    }
}
