//! The stage ↔ stage-helper wire: JSON lines over a unix socket (`helper-v3.sock`).
//!
//! ```text
//! → {"op":"mermaid","id":3,"source":"flowchart TD\n a-->b","max_width":400.0,"font_size":14.0}
//! ← {"id":3,"ok":true,"svg":"<svg…","width":212.5,"height":96.0}
//! ← {"id":3,"ok":false,"error":"…"}
//! ```
//!
//! Requests carry an `id`: new work (`mermaid`, `image`, `html-open`) gets a fresh one that
//! its replies echo; `html-*` requests about a page reuse the id `html-open` was sent with.
//! Fixtures in tests/fixtures/helper/ pin every shape; the Swift mirror is
//! `HelperProtocol.swift` in NotesCore.

use std::path::PathBuf;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// One request line from stage. Unknown ops and fields are errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HelperRequest {
    /// Launches the helper early; answered with [`HelperReply::Done`].
    Warm { id: u64 },
    /// mermaid source → SVG, answered with [`HelperReply::Mermaid`]. `max_width` is in CSS px.
    Mermaid { id: u64, source: String, max_width: f64, font_size: f64 },
    /// Decode an image file to RGBA8 in shm, answered with [`HelperReply::Image`].
    Image { id: u64, path: PathBuf },
    /// Open a page; it then streams [`HelperReply::Frame`]s under this `id`.
    /// `width` and `max_height` are CSS px, `scale` the device pixel ratio.
    HtmlOpen { id: u64, path: PathBuf, width: f64, max_height: f64, scale: f64, fps: f64 },
    /// A new revision of the page's file; answered with [`HelperReply::Patched`].
    HtmlUpdate { id: u64, path: PathBuf },
    /// stage is done with `frame` (its shm is consumed); the next one may be sent.
    HtmlAck { id: u64, frame: u64 },
    HtmlPause { id: u64 },
    HtmlResume { id: u64 },
    HtmlClose { id: u64 },
    HtmlResize { id: u64, width: f64, max_height: f64, scale: f64 },
}

/// A boolean that is always `V` on the wire (`"ok":true` / `"ok":false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flag<const V: bool>;

impl<const V: bool> Serialize for Flag<V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(V)
    }
}

impl<'de, const V: bool> Deserialize<'de> for Flag<V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match bool::deserialize(d)? == V {
            true => Ok(Flag),
            false => Err(de::Error::custom(format!("expected {V}"))),
        }
    }
}

/// One line from the helper: a reply or a frame. Told apart by their fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HelperReply {
    /// A live page's pixels, in a new shm object that stage unlinks.
    Frame(Frame),
    /// Any request failed. `id` is null when the line wasn't even a request.
    Failed(Failed),
    /// `mermaid`: pele's SVG with stage's palette resolved; `width`/`height` in CSS px.
    Mermaid(Mermaid),
    /// `html-update`: whether the new revision was patched into the live page (else it reloads).
    Patched(Patched),
    /// `image`: straight-alpha RGBA8 rows in a new shm object.
    Image(Image),
    /// `warm`.
    Done(Done),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub id: u64,
    pub frame: u64,
    pub shm: String,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failed {
    pub id: Option<u64>,
    pub ok: Flag<false>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mermaid {
    pub id: u64,
    pub ok: Flag<true>,
    pub svg: String,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patched {
    pub id: u64,
    pub ok: Flag<true>,
    pub patched: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub id: u64,
    pub ok: Flag<true>,
    pub shm: String,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Done {
    pub id: u64,
    pub ok: Flag<true>,
}
