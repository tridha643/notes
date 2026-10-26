//! `STAGE_LATENCY_LOG=<file>`: after each paint, one `post_id version painted_ns`
//! line per post row that just appeared, for scripts/stage-latency.sh to join
//! against publish times.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

use notes_proto::posts::PostId;

pub struct Latency(Option<File>);

impl Latency {
    pub fn from_env() -> Latency {
        let file = std::env::var_os("STAGE_LATENCY_LOG")
            .and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());
        Latency(file)
    }

    pub fn painted(&mut self, rows: &[(PostId, u64)]) {
        let Some(file) = &mut self.0 else { return };
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        for (id, version) in rows {
            let _ = writeln!(file, "{id} {version} {ns}");
        }
    }
}
