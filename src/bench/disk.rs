//! RESET BENCH · disco, no estilo do CrystalDiskMark: leitura e escrita sequenciais (blocos de 1 MiB) e acesso
//! aleatório de 4 KiB (1 e 8 requisições em paralelo). Usa `O_DIRECT` + `O_DSYNC` para medir o disco e não o cache de
//! páginas do kernel. O arquivo de teste fica numa pasta própria do ponto de montagem escolhido e é apagado no final.

use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::io::{Access, AlignedBuf, Rng, TempDir, fill_random, open_bench};
use crate::storage;
use crate::util::{Cancel, Roots, can_read_write};

const BLOCK: usize = 1 << 20;
const SMALL: usize = 4096;
/// Tempo de cada fase aleatória no teste oficial.
pub const RANDOM_SECS: f64 = 4.0;
/// Espaço livre mínimo para o teste menor (512 MiB de arquivo + margem).
pub const MIN_FREE: u64 = 1536 << 20;
const FREE_MARGIN: u64 = 512 << 20;
const QD8_THREADS: usize = 8;

#[derive(Debug, Clone, Serialize)]
pub struct DiskBenchResult {
    pub target: String,
    pub file_mb: u64,
    pub seq_read_mbs: f64,
    pub seq_write_mbs: f64,
    pub read_iops_q1: f64,
    pub write_iops_q1: f64,
    pub read_latency_us: f64,
    pub write_latency_us: f64,
    pub read_iops_q8: f64,
    /// `false` quando o sistema de arquivos não aceitou E/S direta (o resultado pode incluir cache).
    pub direct: bool,
}

impl DiskBenchResult {
    /// Nota só para a barra do histórico: o sequencial pesa em cópia e carregamento; o 4K, em abrir programas e sistema.
    pub fn score(&self) -> f64 {
        (self.seq_read_mbs + self.seq_write_mbs) / 2.0 + (self.read_iops_q1 + self.write_iops_q1) / 200.0 + self.read_iops_q8 / 2000.0
    }

    pub fn summary(&self) -> String {
        format!(
            "seq {:.0}/{:.0} MB/s · 4K Q1 {:.0}/{:.0} IOPS · 4K Q8 {:.0} IOPS",
            self.seq_read_mbs, self.seq_write_mbs, self.read_iops_q1, self.write_iops_q1, self.read_iops_q8
        )
    }
}

/// Ponto de montagem onde o teste pode rodar.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub mountpoint: String,
    pub source: String,
    pub fstype: String,
    pub avail: u64,
    pub disk: Option<String>,
}

/// Pontos de montagem com pelo menos 1,5 GiB livres e permissão de escrita para o usuário atual.
pub fn targets(r: &Roots) -> Vec<Target> {
    storage::collect_mounts(r)
        .into_iter()
        .filter(|m| m.avail >= MIN_FREE && can_read_write(Path::new(&m.mountpoint)))
        .map(|m| Target { mountpoint: m.mountpoint, source: m.source, fstype: m.fstype, avail: m.avail, disk: m.disk })
        .collect()
}

fn cancelled() -> String {
    "Teste cancelado.".to_string()
}

/// Fase aleatória com 1 requisição por vez. Devolve (IOPS, latência média em µs).
#[allow(clippy::too_many_arguments)]
fn random_q1(file: &std::fs::File, slots: u64, write: bool, secs: f64, progress: &dyn Fn(&str, f64), phase: &str, start: f64, span: f64, cancel: &Cancel) -> Result<(f64, f64), String> {
    let mut buf = AlignedBuf::new(SMALL);
    fill_random(buf.as_mut_slice(), 11);
    let mut rng = Rng::new(if write { 7 } else { 3 });
    let (mut ops, t0) = (0u64, Instant::now());
    while t0.elapsed().as_secs_f64() < secs {
        // confere o cancelamento a cada 64 operações (barato) e atualiza a barra
        for _ in 0..64 {
            let off = rng.below(slots) * SMALL as u64;
            let r = if write { file.write_all_at(&buf.as_slice()[..SMALL], off) } else { file.read_exact_at(&mut buf.as_mut_slice()[..SMALL], off) };
            r.map_err(|e| format!("falha na E/S aleatória: {e}"))?;
            ops += 1;
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        progress(phase, start + span * (t0.elapsed().as_secs_f64() / secs).min(1.0));
    }
    let el = t0.elapsed().as_secs_f64();
    Ok((ops as f64 / el, el / ops as f64 * 1e6))
}

/// Leitura aleatória de 4 KiB com 8 threads (8 requisições em paralelo). Devolve IOPS.
fn random_read_parallel(file: &std::fs::File, slots: u64, secs: f64, progress: &dyn Fn(&str, f64), cancel: &Cancel) -> Result<f64, String> {
    let deadline = Instant::now() + Duration::from_secs_f64(secs);
    let begin = Instant::now();
    let results: Vec<Result<u64, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..QD8_THREADS)
            .map(|t| {
                s.spawn(move || {
                    let mut buf = AlignedBuf::new(SMALL);
                    let mut rng = Rng::new(100 + t as u64);
                    let mut local = 0u64;
                    while Instant::now() < deadline && !cancel.is_cancelled() {
                        for _ in 0..32 {
                            file.read_exact_at(&mut buf.as_mut_slice()[..SMALL], rng.below(slots) * SMALL as u64).map_err(|e| format!("falha na leitura paralela: {e}"))?;
                            local += 1;
                        }
                    }
                    Ok(local)
                })
            })
            .collect();
        while handles.iter().any(|h| !h.is_finished()) {
            progress("Leitura 4 KiB aleatória (8 em paralelo)", 0.80 + 0.19 * (begin.elapsed().as_secs_f64() / secs).min(1.0));
            std::thread::sleep(Duration::from_millis(100));
        }
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("thread de teste falhou".into()))).collect()
    });
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let mut total = 0u64;
    for r in results {
        total += r?;
    }
    Ok(total as f64 / begin.elapsed().as_secs_f64())
}

/// Roda o benchmark em `mountpoint` com um arquivo de `file_mb` MiB e `random_secs` segundos por fase aleatória.
pub fn run(mountpoint: &Path, file_mb: u64, random_secs: f64, progress: &dyn Fn(&str, f64), cancel: &Cancel) -> Result<DiskBenchResult, String> {
    let bytes = file_mb << 20;
    if bytes < (BLOCK * 4) as u64 {
        return Err("o arquivo de teste precisa ter pelo menos 4 MiB".into());
    }
    match storage::statvfs(&mountpoint.to_string_lossy()) {
        Some((_, _, avail)) if avail < bytes + FREE_MARGIN => {
            return Err(format!("pouco espaço livre em {} (precisa de {} MiB)", mountpoint.display(), file_mb + (FREE_MARGIN >> 20)));
        }
        Some(_) => {}
        None => return Err(format!("não consegui verificar o espaço livre em {}", mountpoint.display())),
    }
    let dir = TempDir::create_in(mountpoint).map_err(|e| format!("não consegui criar a pasta de teste em {}: {e}", mountpoint.display()))?;
    let path = dir.path().join("bench.tmp");
    let bench = open_bench(&path, Access::CreateWrite).map_err(|e| format!("não consegui criar o arquivo de teste: {e}"))?;
    let file = &bench.file;

    let mut seq = AlignedBuf::new(BLOCK);
    fill_random(seq.as_mut_slice(), 42); // dados incompressíveis

    // ---------- Sequencial: escrita ----------
    let t = Instant::now();
    let mut off = 0u64;
    while off < bytes {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        file.write_all_at(&seq.as_slice()[..BLOCK], off).map_err(|e| format!("falha na escrita sequencial: {e}"))?;
        off += BLOCK as u64;
        progress("Escrita sequencial (1 MiB)", 0.25 * off as f64 / bytes as f64);
    }
    let seq_write = file_mb as f64 / t.elapsed().as_secs_f64();
    bench.drop_cache();

    // ---------- Sequencial: leitura ----------
    let t = Instant::now();
    let mut off = 0u64;
    while off < bytes {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        file.read_exact_at(&mut seq.as_mut_slice()[..BLOCK], off).map_err(|e| format!("falha na leitura sequencial: {e}"))?;
        off += BLOCK as u64;
        progress("Leitura sequencial (1 MiB)", 0.25 + 0.25 * off as f64 / bytes as f64);
    }
    let seq_read = file_mb as f64 / t.elapsed().as_secs_f64();

    // ---------- 4 KiB aleatório, 1 por vez ----------
    let slots = bytes / SMALL as u64;
    bench.drop_cache();
    let (read_iops, read_lat) = random_q1(file, slots, false, random_secs, progress, "Leitura 4 KiB aleatória (1 por vez)", 0.50, 0.15, cancel)?;
    let (write_iops, write_lat) = random_q1(file, slots, true, random_secs, progress, "Escrita 4 KiB aleatória (1 por vez)", 0.65, 0.15, cancel)?;

    // ---------- 4 KiB aleatório, 8 em paralelo ----------
    progress("Leitura 4 KiB aleatória (8 em paralelo)", 0.80);
    bench.drop_cache();
    let q8 = random_read_parallel(file, slots, random_secs, progress, cancel)?;

    progress("Concluído", 1.0);
    Ok(DiskBenchResult {
        target: mountpoint.display().to_string(),
        file_mb,
        seq_read_mbs: seq_read,
        seq_write_mbs: seq_write,
        read_iops_q1: read_iops,
        write_iops_q1: write_iops,
        read_latency_us: read_lat,
        write_latency_us: write_lat,
        read_iops_q8: q8,
        direct: bench.direct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn score_follows_the_windows_formula() {
        let r = DiskBenchResult { target: "/".into(), file_mb: 512, seq_read_mbs: 3000.0, seq_write_mbs: 1000.0, read_iops_q1: 10000.0, write_iops_q1: 20000.0, read_latency_us: 100.0, write_latency_us: 50.0, read_iops_q8: 200_000.0, direct: true };
        assert!((r.score() - (2000.0 + 150.0 + 100.0)).abs() < 1e-9);
        assert!(r.summary().contains("seq 3000/1000 MB/s"));
    }

    #[test]
    fn runs_every_phase_and_removes_the_test_file() {
        let base = tempfile::tempdir().unwrap();
        let phases = RefCell::new(Vec::<String>::new());
        let progress = |p: &str, f: f64| {
            assert!((0.0..=1.0).contains(&f), "fração fora de 0..1: {f} em {p}");
            let mut v = phases.borrow_mut();
            if v.last().map(String::as_str) != Some(p) {
                v.push(p.to_string());
            }
        };
        let r = run(base.path(), 8, 0.15, &progress, &Cancel::new()).unwrap();
        for v in [r.seq_read_mbs, r.seq_write_mbs, r.read_iops_q1, r.write_iops_q1, r.read_latency_us, r.write_latency_us, r.read_iops_q8] {
            assert!(v > 0.0 && v.is_finite(), "{r:?}");
        }
        assert_eq!(r.file_mb, 8);
        let p = phases.borrow();
        assert_eq!(p.first().map(String::as_str), Some("Escrita sequencial (1 MiB)"));
        assert_eq!(p.last().map(String::as_str), Some("Concluído"));
        assert_eq!(p.len(), 6, "{p:?}");
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0, "o arquivo de teste tem de ser apagado");
    }

    #[test]
    fn cancel_stops_the_test_and_cleans_up() {
        let base = tempfile::tempdir().unwrap();
        let c = Cancel::new();
        c.cancel();
        let e = run(base.path(), 8, 0.1, &|_, _| {}, &c).unwrap_err();
        assert!(e.contains("cancelado"));
        assert_eq!(std::fs::read_dir(base.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_tiny_files_and_missing_space() {
        let base = tempfile::tempdir().unwrap();
        assert!(run(base.path(), 1, 0.1, &|_, _| {}, &Cancel::new()).unwrap_err().contains("4 MiB"));
        let e = run(base.path(), 1 << 40, 0.1, &|_, _| {}, &Cancel::new()).unwrap_err();
        assert!(e.contains("pouco espaço"), "{e}");
        let e = run(&base.path().join("nao/existe"), 8, 0.1, &|_, _| {}, &Cancel::new()).unwrap_err();
        assert!(e.contains("espaço livre"), "{e}");
    }

    #[test]
    fn lists_writable_mounts_with_enough_space() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("proc")).unwrap();
        std::fs::write(d.path().join("proc/mounts"), "").unwrap();
        assert!(targets(&Roots::under(d.path())).is_empty());
        // com o /proc real, tudo o que aparece tem espaço e permissão
        for t in targets(&Roots::system()) {
            assert!(t.avail >= MIN_FREE);
            assert!(can_read_write(Path::new(&t.mountpoint)));
        }
    }
}
