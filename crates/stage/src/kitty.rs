//! Kitty graphics: escapes for transmitting pixels through POSIX shared memory
//! (`t=s`) and placing them in cells, plus the shared-memory writer. Every
//! escape carries `q=2`, so the terminal never answers into stage's input.

use std::{ffi::CString, io};

use base64::{engine::general_purpose::STANDARD, Engine};

/// Where an image sits: its top-left cell, its size in cells, and the source
/// rectangle (px) shown there, so a card scrolled half off-screen is cropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    pub src: (u32, u32, u32, u32),
}

fn place_keys(p: &Place) -> String {
    let (x, y, w, h) = p.src;
    format!("p=1,x={x},y={y},w={w},h={h},c={},r={},C=1", p.cols, p.rows)
}

fn at(p: &Place) -> String {
    format!("\x1b[{};{}H", p.row + 1, p.col + 1)
}

/// Hand `shm` (RGBA, `w`×`h`) to the terminal as image `id`; it reads and
/// unlinks the object. With `place`, the image also appears there, replacing
/// the previous placement in place (how live frames update).
pub fn transmit(id: u32, shm: &str, (w, h): (u32, u32), place: Option<&Place>) -> Vec<u8> {
    let name = STANDARD.encode(shm);
    match place {
        Some(p) => format!("{}\x1b_Ga=T,t=s,f=32,s={w},v={h},i={id},{},q=2;{name}\x1b\\", at(p), place_keys(p)),
        None => format!("\x1b_Ga=t,t=s,f=32,s={w},v={h},i={id},q=2;{name}\x1b\\"),
    }
    .into_bytes()
}

/// Show an already-transmitted image at `p` (replaces its placement).
pub fn place(id: u32, p: &Place) -> Vec<u8> {
    format!("{}\x1b_Ga=p,i={id},{},q=2\x1b\\", at(p), place_keys(p)).into_bytes()
}

/// Remove the image's placement but keep its pixels for the next `place`.
pub fn unplace(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={id},p=1,q=2\x1b\\").into_bytes()
}

/// Every image and placement stage made (on quit).
pub const FREE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Write RGBA pixels into a fresh shared-memory object and return its name.
/// The terminal unlinks it after a `t=s` transmit; call [`unlink`] otherwise.
pub fn shm_write(rgba: &[u8]) -> io::Result<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    // macOS caps shm names at 31 bytes.
    let name = format!("/stg-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
    let c = CString::new(name.clone()).expect("no NUL");
    // SAFETY: plain POSIX calls on an fd we own; the mapping is written then dropped.
    unsafe {
        let fd = libc::shm_open(c.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600 as libc::c_uint);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fail = |fd| {
            let e = io::Error::last_os_error();
            libc::close(fd);
            libc::shm_unlink(c.as_ptr());
            Err(e)
        };
        if libc::ftruncate(fd, rgba.len() as libc::off_t) != 0 {
            return fail(fd);
        }
        let ptr = libc::mmap(std::ptr::null_mut(), rgba.len(), libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        if ptr == libc::MAP_FAILED {
            return fail(fd);
        }
        std::ptr::copy_nonoverlapping(rgba.as_ptr(), ptr.cast::<u8>(), rgba.len());
        libc::munmap(ptr, rgba.len());
        libc::close(fd);
    }
    Ok(name)
}

/// Read a shared-memory object's first `len` bytes (a helper's frame).
pub fn shm_read(name: &str, len: usize) -> io::Result<Vec<u8>> {
    let c = CString::new(name).map_err(|_| io::Error::other("bad shm name"))?;
    // SAFETY: as above, read-only mapping of `len` bytes we were told exist.
    unsafe {
        let fd = libc::shm_open(c.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let ptr = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
        libc::close(fd);
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let out = std::slice::from_raw_parts(ptr.cast::<u8>(), len).to_vec();
        libc::munmap(ptr, len);
        Ok(out)
    }
}

pub fn unlink(name: &str) {
    if let Ok(c) = CString::new(name) {
        // SAFETY: unlinking a name; harmless if it's already gone.
        unsafe { libc::shm_unlink(c.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_round_trips() {
        let px = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let name = shm_write(&px).unwrap();
        assert!(name.len() <= 31);
        assert_eq!(shm_read(&name, px.len()).unwrap(), px);
        unlink(&name);
        assert!(shm_read(&name, px.len()).is_err());
    }

    #[test]
    fn escapes_are_quiet_and_cropped() {
        let p = Place { col: 4, row: 2, cols: 10, rows: 3, src: (0, 30, 140, 90) };
        let s = String::from_utf8(place(7, &p)).unwrap();
        assert!(s.starts_with("\x1b[3;5H\x1b_Ga=p,i=7,p=1,x=0,y=30,w=140,h=90,c=10,r=3,C=1,q=2"), "{s}");
        let t = String::from_utf8(transmit(7, "/stg-1-0", (140, 120), None)).unwrap();
        assert!(t.contains("a=t,t=s,f=32,s=140,v=120,i=7,q=2;") && t.ends_with("\x1b\\"), "{t}");
    }
}
