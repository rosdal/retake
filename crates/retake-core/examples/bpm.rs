//! Prints the detected tempo of raw mono f32 22.05 kHz files named `<bpm>_<title>.f32`:
//! `cargo run -p retake-core --release --example bpm <dir>`.
fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let mut ok = 0;
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "f32") {
            continue;
        }
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let want: f64 = name.split('_').next().unwrap().parse().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let s: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let a = retake_core::song::analyse(&s).unwrap();
        let good = (a.bpm - want).abs() / want < 0.02;
        n += 1;
        ok += usize::from(good);
        println!(
            "{name}: want {want} got {:.1} {}",
            a.bpm,
            if good { "ok" } else { "MISS" }
        );
    }
    println!("{ok}/{n}");
}
