//! RESET BENCH · memória e carregamento: o que pesa em jogos fora da CPU e da GPU. Latência e banda da RAM, e a
//! velocidade de "carregar um mapa" (um pacote grande + muitos arquivos pequenos) lendo do disco sem o cache do kernel.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::*;
use serde::Serialize;

use super::io::{Access, AlignedBuf, Rng, TempDir, fill_random, open_bench};
use crate::storage;
use crate::util::Cancel;

/// 256 MiB: bem maior que qualquer cache L3.
pub const LATENCY_BYTES: usize = 256 << 20;
/// 1 GiB por buffer (leitura e cópia usam dois buffers).
pub const BANDWIDTH_BYTES: usize = 1 << 30;
/// "Pacote" do mapa.
pub const PACK_BYTES: u64 = 512 << 20;
pub const ASSET_COUNT: usize = 1000;
/// Múltiplo de 4 KiB, como exige a leitura direta.
pub const ASSET_BYTES: usize = 64 << 10;
const ASSET_THREADS: usize = 8;
const SEQ_CHUNK: usize = 4 << 20;
/// Espaço livre extra exigido além dos arquivos de teste.
const FREE_MARGIN: u64 = 256 << 20;

#[derive(Debug, Clone, Serialize)]
pub struct MemoryResult {
    pub latency_ns: f64,
    pub read_gbs: f64,
    pub copy_gbs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadResult {
    pub seq_read_mbs: f64,
    pub small_files_mbs: f64,
    pub files_per_sec: f64,
    /// Estimativa do tempo para carregar o mapa: pacote sequencial + arquivos pequenos.
    pub map_load_seconds: f64,
    /// Pasta onde o teste rodou.
    pub dir: String,
    /// `false` quando o sistema de arquivos não aceitou leitura direta (o resultado pode incluir cache).
    pub direct: bool,
}

// ---------------------------------------------------------------------------------------------
// RAM
// ---------------------------------------------------------------------------------------------

/// Monta uma lista encadeada em ordem aleatória (permutação de Sattolo = um único ciclo) com saltos de 64 bytes
/// (uma linha de cache por salto). `chain[p]` é o índice (em `u64`) da próxima posição a visitar.
pub fn build_chain(words: usize) -> Vec<u64> {
    let lines = words / 8;
    let mut next = vec![0u64; words];
    if lines < 2 {
        return next;
    }
    let mut order: Vec<u32> = (0..lines as u32).collect();
    let mut rng = Rng::new(42);
    for i in (1..lines).rev() {
        let j = rng.below(i as u64) as usize; // j < i: garante um único ciclo
        order.swap(i, j);
    }
    for i in 0..lines {
        next[order[i] as usize * 8] = order[(i + 1) % lines] as u64 * 8;
    }
    next
}

/// Latência da RAM em ns: cada leitura depende da anterior, então o processador não consegue adiantar e o tempo
/// medido é o da ida à memória. Devolve 0 se for cancelado antes de qualquer leitura.
pub fn measure_latency_ns(bytes: usize, steps: u64, cancel: &Cancel) -> f64 {
    let next = build_chain(bytes / 8);
    let mut p = 0usize;
    for _ in 0..200_000.min(steps) {
        p = next[p] as usize; // aquece TLB e páginas
    }
    let block = 1u64 << 20;
    let mut done = 0u64;
    let start = Instant::now();
    while done < steps && !cancel.is_cancelled() {
        let n = block.min(steps - done);
        for _ in 0..n {
            p = next[p] as usize;
        }
        done += n;
    }
    black_box(p);
    if done == 0 { 0.0 } else { start.elapsed().as_secs_f64() * 1e9 / done as f64 }
}

fn sum4(span: &[u64]) -> u64 {
    let (mut s0, mut s1, mut s2, mut s3) = (0u64, 0u64, 0u64, 0u64);
    let mut it = span.chunks_exact(4);
    for c in &mut it {
        s0 = s0.wrapping_add(c[0]);
        s1 = s1.wrapping_add(c[1]);
        s2 = s2.wrapping_add(c[2]);
        s3 = s3.wrapping_add(c[3]);
    }
    for v in it.remainder() {
        s0 = s0.wrapping_add(*v);
    }
    s0.wrapping_add(s1).wrapping_add(s2).wrapping_add(s3)
}

/// Banda de leitura (soma) e de cópia com todas as threads, em GB/s (melhor de 3).
pub fn measure_bandwidth(bytes: usize, cancel: &Cancel) -> (f64, f64) {
    let words = bytes / 8;
    let threads = rayon::current_num_threads().max(1);
    let chunk = words.div_ceil(threads).max(4);
    let mut src = vec![0u64; words];
    let mut dst = vec![0u64; words];
    src.par_chunks_mut(chunk).enumerate().for_each(|(t, c)| c.fill(t as u64 + 1));
    dst.par_chunks_mut(chunk).for_each(|c| c.fill(0)); // força a alocação das páginas fora da medição

    let (mut best_read, mut best_copy) = (0.0f64, 0.0f64);
    for _ in 0..3 {
        if cancel.is_cancelled() {
            break;
        }
        let t = Instant::now();
        let total = src.par_chunks(chunk).map(sum4).reduce(|| 0u64, u64::wrapping_add);
        black_box(total);
        best_read = best_read.max(bytes as f64 / t.elapsed().as_secs_f64() / 1e9);

        let t = Instant::now();
        dst.par_chunks_mut(chunk).zip(src.par_chunks(chunk)).for_each(|(d, s)| d.copy_from_slice(s));
        black_box(dst[words / 2]);
        // Cópia lê e escreve: conta os dois sentidos, como os benchmarks de memória fazem.
        best_copy = best_copy.max(2.0 * bytes as f64 / t.elapsed().as_secs_f64() / 1e9);
    }
    (best_read, best_copy)
}

// ---------------------------------------------------------------------------------------------
// Carregamento de mapa
// ---------------------------------------------------------------------------------------------

fn asset_path(dir: &Path, i: usize) -> std::path::PathBuf {
    dir.join(format!("asset{i:04}.dat"))
}

/// Lê todos os assets com `threads` threads, cada uma com seu buffer alinhado.
fn read_assets(dir: &Path, count: usize, bytes: usize, threads: usize, cancel: &Cancel) -> Result<bool, String> {
    let all_direct = std::sync::atomic::AtomicBool::new(true);
    let failure = std::sync::Mutex::new(None::<String>);
    std::thread::scope(|s| {
        for t in 0..threads {
            let (all_direct, failure) = (&all_direct, &failure);
            s.spawn(move || {
                let mut buf = AlignedBuf::new(bytes);
                let mut i = t;
                while i < count && !cancel.is_cancelled() {
                    match open_bench(&asset_path(dir, i), Access::ReadOnly) {
                        Ok(f) => {
                            if !f.direct {
                                all_direct.store(false, std::sync::atomic::Ordering::Relaxed);
                            }
                            if let Err(e) = f.file.read_exact_at(&mut buf.as_mut_slice()[..bytes], 0) {
                                *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("falha ao ler o asset {i}: {e}"));
                                return;
                            }
                        }
                        Err(e) => {
                            *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("falha ao abrir o asset {i}: {e}"));
                            return;
                        }
                    }
                    i += threads;
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    Ok(all_direct.load(std::sync::atomic::Ordering::Relaxed))
}

/// Cria os arquivos de teste em `parent`, lê sem cache e apaga tudo no final. `progress` recebe (fase, fração).
/// `pack_bytes` deve ser múltiplo de 4 MiB e `asset_bytes` múltiplo de 4 KiB.
pub fn measure_loading(parent: &Path, pack_bytes: u64, asset_count: usize, asset_bytes: usize, progress: &dyn Fn(&str, f64), cancel: &Cancel) -> Result<LoadResult, String> {
    let need = pack_bytes + asset_count as u64 * asset_bytes as u64 + FREE_MARGIN;
    match storage::statvfs(&parent.to_string_lossy()) {
        Some((_, _, avail)) if avail < need => {
            return Err(format!("pouco espaço livre em {} (precisa de {} MiB)", parent.display(), need >> 20));
        }
        Some(_) => {}
        None => return Err(format!("não consegui verificar o espaço livre em {}", parent.display())),
    }
    let dir = TempDir::create_in(parent).map_err(|e| format!("não consegui criar a pasta de teste em {}: {e}", parent.display()))?;
    let cancelled = || "Teste cancelado.".to_string();

    // 1) Preparação (escrita): não entra na nota.
    let total_prep = (pack_bytes + asset_count as u64 * asset_bytes as u64) as f64;
    let mut block = AlignedBuf::new(SEQ_CHUNK);
    fill_random(block.as_mut_slice(), 7);
    let pack = dir.path().join("mapa.pak");
    {
        let mut f = File::create(&pack).map_err(|e| format!("não consegui criar o pacote de teste: {e}"))?;
        let mut w = 0u64;
        while w < pack_bytes {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            f.write_all(&block.as_slice()[..SEQ_CHUNK.min((pack_bytes - w) as usize)]).map_err(|e| format!("falha ao gravar o pacote de teste: {e}"))?;
            w += SEQ_CHUNK as u64;
            progress("Preparando arquivos de teste", (w.min(pack_bytes)) as f64 / total_prep);
        }
        f.sync_all().map_err(|e| format!("falha ao sincronizar o pacote de teste: {e}"))?;
    }
    for i in 0..asset_count {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        std::fs::write(asset_path(dir.path(), i), &block.as_slice()[..asset_bytes]).map_err(|e| format!("falha ao gravar o asset {i}: {e}"))?;
        if i % 50 == 0 {
            progress("Preparando arquivos de teste", (pack_bytes as f64 + i as f64 * asset_bytes as f64) / total_prep);
        }
    }

    // 2) Leitura sequencial do pacote, sem cache.
    let bench = open_bench(&pack, Access::ReadOnly).map_err(|e| format!("não consegui abrir o pacote de teste: {e}"))?;
    let mut direct = bench.direct;
    let mut buf = AlignedBuf::new(SEQ_CHUNK);
    let start = Instant::now();
    let mut off = 0u64;
    while off < pack_bytes {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let n = SEQ_CHUNK.min((pack_bytes - off) as usize);
        bench.file.read_exact_at(&mut buf.as_mut_slice()[..n], off).map_err(|e| format!("falha ao ler o pacote de teste: {e}"))?;
        off += n as u64;
        progress("Lendo pacote do mapa", off as f64 / pack_bytes as f64);
    }
    let seq_mbs = pack_bytes as f64 / 1_048_576.0 / start.elapsed().as_secs_f64();
    drop(bench);

    // 3) Muitos arquivos pequenos (texturas, sons, scripts): abrir + ler + fechar, com 8 threads como as engines
    //    modernas. A primeira passada (não medida) deixa nomes e inodes em cache, para medir só a leitura dos dados.
    progress("Aquecendo a lista de arquivos", 0.0);
    read_assets(dir.path(), asset_count, asset_bytes, ASSET_THREADS, cancel)?;
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    progress("Lendo assets pequenos (8 threads)", 0.5);
    let start = Instant::now();
    direct &= read_assets(dir.path(), asset_count, asset_bytes, ASSET_THREADS, cancel)?;
    let secs = start.elapsed().as_secs_f64();
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    progress("Concluído", 1.0);
    Ok(LoadResult {
        seq_read_mbs: seq_mbs,
        small_files_mbs: asset_count as f64 * asset_bytes as f64 / 1_048_576.0 / secs,
        files_per_sec: asset_count as f64 / secs,
        map_load_seconds: pack_bytes as f64 / 1_048_576.0 / seq_mbs + secs,
        dir: parent.display().to_string(),
        direct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn chain_is_a_single_cycle_over_all_cache_lines() {
        let words = 8 * 500;
        let next = build_chain(words);
        let (mut p, mut seen) = (0usize, vec![false; 500]);
        for step in 0..500 {
            assert_eq!(p % 8, 0, "todo salto cai no início de uma linha de cache");
            assert!(!seen[p / 8], "linha repetida no passo {step}");
            seen[p / 8] = true;
            p = next[p] as usize;
        }
        assert_eq!(p, 0, "depois de percorrer todas as linhas volta ao início");
        assert!(seen.iter().all(|v| *v));
        assert_eq!(build_chain(8), vec![0; 8], "com uma linha só não há o que encadear");
    }

    #[test]
    fn chain_is_deterministic() {
        assert_eq!(build_chain(8 * 64), build_chain(8 * 64));
    }

    #[test]
    fn sum4_matches_the_plain_sum() {
        let v: Vec<u64> = (1..=1003).collect();
        assert_eq!(sum4(&v), v.iter().sum::<u64>());
        assert_eq!(sum4(&[u64::MAX, 2]), 1, "soma dá a volta sem estourar");
        assert_eq!(sum4(&[]), 0);
    }

    #[test]
    fn latency_is_positive_and_cancellable() {
        let ns = measure_latency_ns(4 << 20, 400_000, &Cancel::new());
        assert!(ns > 0.0 && ns.is_finite() && ns < 100_000.0, "ns = {ns}");
        let c = Cancel::new();
        c.cancel();
        assert_eq!(measure_latency_ns(4 << 20, 400_000, &c), 0.0);
    }

    #[test]
    fn bandwidth_is_positive() {
        let (r, c) = measure_bandwidth(8 << 20, &Cancel::new());
        assert!(r > 0.0 && c > 0.0 && r.is_finite() && c.is_finite(), "r = {r}, c = {c}");
        let k = Cancel::new();
        k.cancel();
        assert_eq!(measure_bandwidth(8 << 20, &k), (0.0, 0.0));
    }

    #[test]
    fn loading_runs_reports_progress_and_cleans_up() {
        let base = tempfile::tempdir().unwrap();
        let phases = RefCell::new(Vec::<String>::new());
        let progress = |p: &str, f: f64| {
            assert!((0.0..=1.0).contains(&f), "fração fora de 0..1: {f} em {p}");
            let mut v = phases.borrow_mut();
            if v.last().map(String::as_str) != Some(p) {
                v.push(p.to_string());
            }
        };
        let r = measure_loading(base.path(), 8 << 20, 40, 8192, &progress, &Cancel::new()).unwrap();
        assert!(r.seq_read_mbs > 0.0 && r.small_files_mbs > 0.0 && r.files_per_sec > 0.0 && r.map_load_seconds > 0.0);
        let p = phases.borrow();
        assert_eq!(p.first().map(String::as_str), Some("Preparando arquivos de teste"));
        assert_eq!(p.last().map(String::as_str), Some("Concluído"));
        assert!(p.iter().any(|x| x == "Lendo pacote do mapa"));
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0, "a pasta de teste tem de ser apagada");
    }

    #[test]
    fn cancelled_loading_cleans_up_and_reports_it() {
        let base = tempfile::tempdir().unwrap();
        let c = Cancel::new();
        c.cancel();
        let e = measure_loading(base.path(), 8 << 20, 10, 4096, &|_, _| {}, &c).unwrap_err();
        assert!(e.contains("cancelado"));
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0);
    }

    #[test]
    fn loading_refuses_when_there_is_not_enough_space() {
        let base = tempfile::tempdir().unwrap();
        let e = measure_loading(base.path(), 1 << 60, 10, 4096, &|_, _| {}, &Cancel::new()).unwrap_err();
        assert!(e.contains("pouco espaço"), "{e}");
        let e2 = measure_loading(&base.path().join("nao/existe"), 8 << 20, 10, 4096, &|_, _| {}, &Cancel::new()).unwrap_err();
        assert!(e2.contains("espaço livre"), "{e2}");
    }
}
