//! Which conversation is calling: the agent process above this `notes` (its
//! first ancestor that isn't a shell or wrapper), identified by pid and start
//! time. notesd gives each one its own session, as sideshow does, instead of
//! one session per folder shared by every conversation run there.

/// Processes between an agent and `notes` that say nothing about who's calling.
const PASS_THROUGH: &[&str] =
    &["zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "env", "timeout", "nohup", "xargs", "time", "sudo", "notes"];

/// `pid:start` of the calling agent, or `None` when the process table can't be
/// read. `NOTES_OWNER` overrides it (empty: no owner, so the per-folder session).
pub fn key() -> Option<String> {
    if let Ok(owner) = std::env::var("NOTES_OWNER") {
        return (!owner.is_empty()).then_some(owner);
    }
    let mut pid = std::os::unix::process::parent_id() as i32;
    for _ in 0..16 {
        let (ppid, name, start) = info(pid)?;
        if !PASS_THROUGH.contains(&name.trim_start_matches('-')) {
            return Some(format!("{pid}:{start}"));
        }
        pid = ppid;
    }
    None
}

/// (parent pid, name, start time in seconds) for `pid`.
fn info(pid: i32) -> Option<(i32, String, u64)> {
    let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `bsd`.
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut bsd).cast(), size) };
    if got != size {
        return None;
    }
    let name: Vec<u8> = bsd.pbi_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Some((bsd.pbi_ppid as i32, String::from_utf8_lossy(&name).into_owned(), bsd.pbi_start_tvsec))
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_a_stable_caller() {
        // Under `cargo test` the caller is cargo (or the harness), never a shell.
        let a = super::key().expect("process table readable");
        assert_eq!(super::key(), Some(a.clone()));
        assert!(a.contains(':'), "{a}");
    }
}
