//! Where stage's bootstrap time goes: `cargo run --release -p stage --example boot -- <session>`.
use std::time::Instant;

use stage::{app::{App, Effect, Msg}, client::StageClient};

fn main() {
    let session = std::env::args().nth(1).map(Into::into);
    let mut client = StageClient::new(notes_proto::default_socket_path());
    let mut app = App::new(session);
    app.size = (120, 40);
    let mut queue: std::collections::VecDeque<Effect> = app.start().into();
    let t0 = Instant::now();
    while let Some(e) = queue.pop_front() {
        if let Effect::Request(req) = e {
            let t = Instant::now();
            let reply = client.perform(&req);
            let io = t.elapsed();
            let t = Instant::now();
            let fx = app.update(Msg::Reply(reply));
            println!("{:<60} io {:6.2} ms  update {:7.2} ms", format!("{req:?}").chars().take(60).collect::<String>(), io.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3);
            queue.extend(fx);
        }
    }
    let t = Instant::now();
    let n = app.feed().lines.len();
    println!("feed {n} lines {:.2} ms; total {:.2} ms", t.elapsed().as_secs_f64() * 1e3, t0.elapsed().as_secs_f64() * 1e3);
}
