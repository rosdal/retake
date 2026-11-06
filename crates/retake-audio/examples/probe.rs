fn main() {
    let pid: u32 = std::env::args()
        .nth(1)
        .map(|s| s.parse().unwrap())
        .unwrap_or(std::process::id());
    eprintln!("starting process loopback on {pid}");
    let c = retake_audio::Capture::start(retake_audio::Source::ProcessLoopback { pid }, |at, f| {
        eprintln!("packet at {at}: {} frames", f.len() / 2);
    });
    match &c {
        Ok(c) => eprintln!("started: {}", c.description()),
        Err(e) => eprintln!("error: {e:#}"),
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    eprintln!("dropping");
    drop(c);
    eprintln!("done");
}
