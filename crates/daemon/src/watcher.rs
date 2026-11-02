//! One blocking kqueue thread on the session folder: vnode events, then `Daemon::stage_refresh`.
//! `kevent` has a NULL timeout, so nothing runs until the folder changes.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::store::Daemon;

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

/// Watch the session folder (flat: session files plus `assets/`). Any change to
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
