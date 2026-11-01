//! Kitty graphics: escapes for transmitting pixels through POSIX shared memory
//! (`t=s`) and placing them in cells, plus the shared-memory writer. Every
//! escape carries `q=2`, so the terminal never answers into stage's input.

use std::{
    ffi::CString,
    io::{self, Write},
};

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

/// Hand the terminal a PNG file as image `id`: it reads and decodes the file
/// itself (`t=f,f=100`), so stage never decodes or copies the pixels.
pub fn transmit_png(id: u32, path: &std::path::Path) -> Vec<u8> {
    let name = STANDARD.encode(path.as_os_str().as_encoded_bytes());
    format!("\x1b_Ga=t,t=f,f=100,i={id},q=2;{name}\x1b\\").into_bytes()
}

/// A PNG's pixel size from its IHDR chunk, or `None` if it isn't a PNG.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Some((be(&bytes[16..20]), be(&bytes[20..24])))
}

/// Show an already-transmitted image at `p` (replaces its placement).
pub fn place(id: u32, p: &Place) -> Vec<u8> {
    format!("{}\x1b_Ga=p,i={id},{},q=2\x1b\\", at(p), place_keys(p)).into_bytes()
}

/// Remove the image's placement but keep its pixels for the next `place`.
pub fn unplace(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={id},p=1,q=2\x1b\\").into_bytes()
}

/// Delete an image and all its placements, freeing its pixels (`I`: lowercase would
/// only remove the placements and leave the pixels in the terminal's memory).
pub fn delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

/// Every image and placement stage made (on quit).
pub const FREE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Terminal output, buffered until `flush` writes it in one go. On Rex, an APC
/// (kitty graphics) escape split across one of its 1024-byte pty reads is
/// misparsed and its tail lands on screen as text, so `flush` pads with NULs
/// (which terminals ignore) to keep each APC inside one 1024-byte block, and
/// pads the whole write to whole blocks so the next write starts aligned too.
pub struct Out {
    buf: Vec<u8>,
    align: bool,
}

impl Default for Out {
    fn default() -> Self {
        Out { buf: Vec::with_capacity(64 * 1024), align: std::env::var("TERM_PROGRAM").is_ok_and(|name| name == "rex") }
    }
}

impl Write for Out {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut out = io::stdout().lock();
        if self.align {
            out.write_all(&align_apcs(&self.buf))?;
        } else {
            out.write_all(&self.buf)?;
        }
        self.buf.clear();
        out.flush()
    }
}

const READ_BLOCK: usize = 1024;

/// `bytes` with NULs added so no APC escape crosses a `READ_BLOCK` boundary and
/// the total is a whole number of blocks.
pub fn align_apcs(bytes: &[u8]) -> Vec<u8> {
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let pad_to_block = |out: &mut Vec<u8>| {
        let used = out.len() % READ_BLOCK;
        if used != 0 {
            out.resize(out.len() + READ_BLOCK - used, 0);
        }
    };
    let mut out = Vec::with_capacity(bytes.len() + 2 * READ_BLOCK);
    let mut rest = bytes;
    while let Some(start) = find(rest, b"\x1b_") {
        let len = find(&rest[start..], b"\x1b\\").map_or(rest.len() - start, |end| end + 2);
        out.extend_from_slice(&rest[..start]);
        if len <= READ_BLOCK && out.len() % READ_BLOCK + len > READ_BLOCK {
            pad_to_block(&mut out);
        }
        out.extend_from_slice(&rest[start..start + len]);
        rest = &rest[start + len..];
    }
    out.extend_from_slice(rest);
    pad_to_block(&mut out);
    out
}

/// Write RGBA pixels into a fresh shared-memory object and return its name.
/// The terminal unlinks it after a `t=s` transmit; call [`unlink`] otherwise.
pub fn shm_write(rgba: &[u8]) -> io::Result<String> {
    shm_fill(rgba.len(), |buf| buf.copy_from_slice(rgba))
}

/// A fresh shared-memory object of `len` zeroed bytes, filled in place by `fill`
/// (so a raster is drawn straight into what the terminal reads, never copied).
pub fn shm_fill(len: usize, fill: impl FnOnce(&mut [u8])) -> io::Result<String> {
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
        if libc::ftruncate(fd, len as libc::off_t) != 0 {
            return fail(fd);
        }
        let ptr = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        if ptr == libc::MAP_FAILED {
            return fail(fd);
        }
        fill(std::slice::from_raw_parts_mut(ptr.cast::<u8>(), len));
        libc::munmap(ptr, len);
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
    fn aligned_output_keeps_every_apc_inside_one_read_block() {
        let apc = String::from_utf8(place(7, &Place { col: 4, row: 2, cols: 53, rows: 38, src: (0, 30, 140, 90) })).unwrap();
        let mut frame = Vec::new();
        for i in 0..200 {
            frame.extend_from_slice(format!("\x1b[{};1Hrow {i} text", i % 40 + 1).as_bytes());
            frame.extend_from_slice(apc.as_bytes());
        }
        let out = align_apcs(&frame);
        assert_eq!(out.len() % READ_BLOCK, 0);
        assert_eq!(out.iter().filter(|&&b| b != 0).copied().collect::<Vec<_>>(), frame, "only NULs are added");
        let mut at = 0;
        while let Some(start) = out[at..].windows(2).position(|w| w == b"\x1b_").map(|p| p + at) {
            let end = start + out[start..].windows(2).position(|w| w == b"\x1b\\").unwrap() + 2;
            assert_eq!(start / READ_BLOCK, (end - 1) / READ_BLOCK, "APC at {start}..{end} crosses a block");
            at = end;
        }
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
