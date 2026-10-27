use pingu::bench::{disk, memory};
use pingu::util::{Cancel, Roots};
fn main() {
    let c = Cancel::default();
    println!("latência 64 MiB: {:.1} ns", memory::measure_latency_ns(64 << 20, 2_000_000, &c));
    let (r, w) = memory::measure_bandwidth(256 << 20, &c);
    println!("banda leitura {:.1} GB/s  escrita {:.1} GB/s", r, w);
    let t = std::env::temp_dir();
    let l = memory::measure_loading(&t, 64 << 20, 200, 64 << 10, &|s, f| eprintln!("  load {s} {:.0}%", f * 100.0), &c);
    println!("loading: {:?}", l);
    for t in disk::targets(&Roots::system()) { println!("alvo: {:?}", t); }
    let d = disk::run(&t, 64, 1.0, &|s, f| eprintln!("  disk {s} {:.0}%", f * 100.0), &c);
    println!("disco: {:?}", d);
    let left = std::fs::read_dir(&t).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().starts_with(".pingu-bench")).count();
    println!("sobras no tmp: {left}");
}
