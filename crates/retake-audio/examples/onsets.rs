//! Prints the capture time (Unix µs) of each loud onset in a process's loopback audio:
//! `cargo run -p retake-audio --example onsets -- <pid> <seconds>`.
fn main() {
    let mut a = std::env::args().skip(1);
    let pid: u32 = a.next().expect("pid").parse().unwrap();
    let secs: u64 = a.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let quiet = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(48_000));
    let q = quiet.clone();
    let _c = retake_audio::Capture::start(
        retake_audio::Source::ProcessLoopback { pid },
        move |at, f| {
            for (i, s) in f.as_chunks::<2>().0.iter().enumerate() {
                if s[0].unsigned_abs() > 3000 {
                    if q.load(std::sync::atomic::Ordering::Relaxed) > 14_400 {
                        println!("onset {}", at + i as i64 * 1_000_000 / 48_000);
                    }
                    q.store(0, std::sync::atomic::Ordering::Relaxed);
                } else {
                    q.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(secs));
}
