//! Stress test de CPU (ponto flutuante + inteiros em todos os threads) e de RAM (grava e verifica padrões).
//! Para sozinho quando a temperatura chega ao limite, o tempo acaba, aparece erro de memória ou o usuário cancela.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::sensors::{self, CpuSampler, RaplSampler};
use crate::util::*;

const CHUNK_WORDS: usize = 16 * 1024 * 1024; // 128 MiB por bloco

#[derive(Debug, Clone)]
pub struct StressConfig {
    pub cpu: bool,
    pub ram: bool,
    /// Fração da RAM disponível a usar (0,1 a 0,9).
    pub ram_fraction: f64,
    /// Orçamento de RAM em bytes. Se `None`, calculado a partir da memória disponível.
    pub ram_bytes: Option<u64>,
    /// Temperatura da CPU (°C) que interrompe o teste.
    pub max_temp: f64,
    pub duration: Duration,
    pub sample_interval: Duration,
}

impl Default for StressConfig {
    fn default() -> Self {
        StressConfig { cpu: true, ram: false, ram_fraction: 0.6, ram_bytes: None, max_temp: 90.0, duration: Duration::from_secs(300), sample_interval: Duration::from_secs(1) }
    }
}

/// Bytes de RAM a usar: fração da memória disponível, limitada entre 10% e 90%.
pub fn plan_ram_bytes(available: u64, fraction: f64) -> u64 {
    (available as f64 * fraction.clamp(0.1, 0.9)) as u64
}

// ---------------------------------------------------------------------------------------------
// Trabalho
// ---------------------------------------------------------------------------------------------

/// Padrão de uma passada de memória.
pub fn pattern_for(pass: u64) -> u64 {
    pass.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

pub fn fill_chunk(c: &mut [u64], pattern: u64) {
    for (i, w) in c.iter_mut().enumerate() {
        *w = pattern ^ i as u64;
    }
}

/// Palavras diferentes do padrão esperado.
pub fn count_errors(c: &[u64], pattern: u64) -> u64 {
    let c = black_box(c);
    c.iter().enumerate().filter(|(i, w)| **w != pattern ^ *i as u64).count() as u64
}

#[derive(Debug, Default)]
struct Shared {
    cpu_ops: AtomicU64,
    mem_errors: AtomicU64,
    mem_passes: AtomicU64,
    mem_bytes: AtomicU64,
}

/// Reduz a prioridade do thread atual (nice 10), para a interface continuar respondendo com os núcleos a 100%.
fn lower_priority() {
    // SAFETY: gettid e setpriority não têm pré-condições além de argumentos válidos.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

fn cpu_worker(seed: usize, cancel: &Cancel, sh: &Shared) {
    lower_priority();
    let mut buf = vec![0f64; 4096];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i + seed + 1) as f64;
    }
    let mut x: u64 = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    while !cancel.is_cancelled() {
        for _ in 0..64 {
            for (i, slot) in buf.iter_mut().enumerate() {
                let mut v = *slot;
                v = v.mul_add(1.000_000_1, 0.5);
                v = (v * v + 1.0).sqrt() + v.sin() * 1e-3;
                *slot = if v > 1e12 { (i + 1) as f64 } else { v };
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
            }
        }
        sh.cpu_ops.fetch_add(64 * 4096, Relaxed);
        black_box((&buf, x));
    }
}

/// Aloca blocos sem abortar o processo se faltar memória.
fn alloc_chunks(total: u64, cancel: &Cancel) -> Vec<Vec<u64>> {
    let mut chunks: Vec<Vec<u64>> = Vec::new();
    let mut left = total as usize / 8;
    while left > 0 && !cancel.is_cancelled() {
        let words = left.min(CHUNK_WORDS);
        let mut v: Vec<u64> = Vec::new();
        if v.try_reserve_exact(words).is_err() {
            break;
        }
        v.resize(words, 0);
        chunks.push(v);
        left -= words;
    }
    chunks
}

fn ram_worker(total: u64, cancel: &Cancel, sh: &Shared) {
    lower_priority();
    let mut chunks = alloc_chunks(total, cancel);
    let bytes: u64 = chunks.iter().map(|c| c.len() as u64 * 8).sum();
    sh.mem_bytes.store(bytes, Relaxed);
    if chunks.is_empty() {
        return;
    }
    let mut pass: u64 = 0;
    while !cancel.is_cancelled() {
        pass += 1;
        let pattern = pattern_for(pass);
        for c in chunks.iter_mut() {
            fill_chunk(c, pattern);
            if cancel.is_cancelled() {
                return;
            }
        }
        for c in &chunks {
            let e = count_errors(c, pattern);
            if e > 0 {
                sh.mem_errors.fetch_add(e, Relaxed);
            }
            if cancel.is_cancelled() {
                return;
            }
        }
        sh.mem_passes.fetch_add(1, Relaxed);
    }
}

// ---------------------------------------------------------------------------------------------
// Leitura de sensores durante o teste
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeReading {
    pub cpu_temp: Option<f64>,
    pub cpu_load: Option<f64>,
    pub power_w: Option<f64>,
    pub clock_mhz: Option<f64>,
}

pub trait Probe {
    fn read(&mut self) -> ProbeReading;
}

/// Lê os sensores reais do sistema.
pub struct SystemProbe {
    roots: Roots,
    cpu: CpuSampler,
    rapl: RaplSampler,
}

impl SystemProbe {
    pub fn new() -> Self {
        let roots = Roots::system();
        SystemProbe { cpu: CpuSampler::new(&roots), rapl: RaplSampler::new(&roots), roots }
    }

    /// Motivo de não haver leitura de consumo.
    pub fn power_note(&self) -> Option<&str> {
        self.rapl.note.as_deref()
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe for SystemProbe {
    fn read(&mut self) -> ProbeReading {
        let temps = sensors::read_all(&self.roots);
        let clocks = sensors::cpu_clocks(&self.roots);
        ProbeReading {
            cpu_temp: sensors::cpu_temperature(&temps),
            cpu_load: Some(self.cpu.sample().total),
            power_w: self.rapl.sample(),
            clock_mhz: (!clocks.is_empty()).then(|| clocks.iter().sum::<f64>() / clocks.len() as f64),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Execução
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StopReason {
    Time,
    Temperature(f64),
    MemoryErrors(u64),
    Cancelled,
    /// Não foi possível iniciar (ex.: sem memória para o teste).
    Failed(String),
}

impl StopReason {
    pub fn text(&self) -> String {
        match self {
            StopReason::Time => "Tempo concluído".into(),
            StopReason::Temperature(t) => format!("Parou: a CPU chegou a {t:.0} °C (limite de segurança)"),
            StopReason::MemoryErrors(n) => format!("Parou: {n} erro(s) de memória detectado(s)"),
            StopReason::Cancelled => "Cancelado pelo usuário".into(),
            StopReason::Failed(e) => format!("Falhou: {e}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StressSample {
    pub t: f64,
    pub cpu_temp: Option<f64>,
    pub cpu_load: Option<f64>,
    pub power_w: Option<f64>,
    pub clock_mhz: Option<f64>,
    pub mem_passes: u64,
    pub mem_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StressReport {
    pub reason: StopReason,
    pub seconds: f64,
    pub samples: Vec<StressSample>,
    pub max_temp: Option<f64>,
    pub avg_load: Option<f64>,
    pub avg_power: Option<f64>,
    pub mem_bytes: u64,
    pub mem_passes: u64,
    pub mem_errors: u64,
    pub log: Vec<String>,
}

impl StressReport {
    /// O teste terminou sem problemas (tempo completo, sem erros de memória)?
    pub fn passed(&self) -> bool {
        self.reason == StopReason::Time && self.mem_errors == 0
    }
}

fn avg(v: impl Iterator<Item = f64>) -> Option<f64> {
    let (s, n) = v.fold((0.0, 0u32), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| s / n as f64)
}

/// Executa o teste (bloqueia até terminar). `progress` recebe cada amostra.
pub fn run(cfg: &StressConfig, probe: &mut dyn Probe, cancel: &Cancel, progress: &dyn Fn(&StressSample)) -> StressReport {
    let shared = Arc::new(Shared::default());
    let stop = Cancel::new();
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    let mut log: Vec<String> = Vec::new();
    let started = Instant::now();

    let logical = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let cpu_threads = if cfg.cpu && cfg.ram && logical > 1 { logical - 1 } else { logical };
    if cfg.cpu {
        for i in 0..cpu_threads {
            let (s, sh) = (stop.clone(), shared.clone());
            handles.push(thread::spawn(move || cpu_worker(i, &s, &sh)));
        }
        log.push(format!("Carga de CPU em {cpu_threads} thread(s)."));
    }
    if cfg.ram {
        let bytes = cfg.ram_bytes.unwrap_or_else(|| plan_ram_bytes(crate::sysinfo::collect_mem(&Roots::system(), false).available, cfg.ram_fraction));
        let (s, sh) = (stop.clone(), shared.clone());
        handles.push(thread::spawn(move || ram_worker(bytes, &s, &sh)));
        log.push(format!("Teste de RAM com até {} (padrões gravados e verificados).", fmt_bytes(bytes)));
    }
    if handles.is_empty() {
        return StressReport { reason: StopReason::Failed("nenhum teste selecionado".into()), seconds: 0.0, samples: vec![], max_temp: None, avg_load: None, avg_power: None, mem_bytes: 0, mem_passes: 0, mem_errors: 0, log };
    }

    let mut samples: Vec<StressSample> = Vec::new();
    let mut reason = StopReason::Time;
    let mut ram_start_reported = false;
    probe.read(); // descarta a primeira leitura (carga e consumo precisam de dois pontos)
    loop {
        if cancel.sleep(cfg.sample_interval) {
            reason = StopReason::Cancelled;
            break;
        }
        let r = probe.read();
        let s = StressSample { t: started.elapsed().as_secs_f64(), cpu_temp: r.cpu_temp, cpu_load: r.cpu_load, power_w: r.power_w, clock_mhz: r.clock_mhz, mem_passes: shared.mem_passes.load(Relaxed), mem_errors: shared.mem_errors.load(Relaxed) };
        progress(&s);
        let (temp, errs) = (s.cpu_temp, s.mem_errors);
        samples.push(s);
        if cfg.ram && !ram_start_reported && shared.mem_bytes.load(Relaxed) > 0 {
            ram_start_reported = true;
            log.push(format!("RAM alocada: {}.", fmt_bytes(shared.mem_bytes.load(Relaxed))));
        }
        if errs > 0 {
            reason = StopReason::MemoryErrors(errs);
            break;
        }
        if let Some(t) = temp.filter(|t| *t >= cfg.max_temp) {
            reason = StopReason::Temperature(t);
            break;
        }
        if started.elapsed() >= cfg.duration {
            break;
        }
        if cfg.ram && !cfg.cpu && started.elapsed() > Duration::from_secs(3) && shared.mem_bytes.load(Relaxed) == 0 && shared.mem_passes.load(Relaxed) == 0 && handles.iter().all(|h| h.is_finished()) {
            reason = StopReason::Failed("não consegui reservar memória para o teste".into());
            break;
        }
    }
    stop.cancel();
    for h in handles {
        let _ = h.join();
    }
    // Uma verificação que terminou entre a última amostra e o encerramento também conta.
    let final_errors = shared.mem_errors.load(Relaxed);
    if final_errors > 0 && !matches!(reason, StopReason::MemoryErrors(_)) {
        reason = StopReason::MemoryErrors(final_errors);
    }
    log.push(reason.text());
    StressReport {
        seconds: started.elapsed().as_secs_f64(),
        max_temp: samples.iter().filter_map(|s| s.cpu_temp).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v)))),
        avg_load: avg(samples.iter().filter_map(|s| s.cpu_load)),
        avg_power: avg(samples.iter().filter_map(|s| s.power_w)),
        mem_bytes: shared.mem_bytes.load(Relaxed),
        mem_passes: shared.mem_passes.load(Relaxed),
        mem_errors: final_errors,
        samples,
        reason,
        log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        temp: Option<f64>,
        reads: u32,
    }

    impl Probe for Fake {
        fn read(&mut self) -> ProbeReading {
            self.reads += 1;
            ProbeReading { cpu_temp: self.temp, cpu_load: Some(99.0), power_w: Some(65.0), clock_mhz: Some(4000.0) }
        }
    }

    fn quick(cpu: bool, ram: bool) -> StressConfig {
        StressConfig { cpu, ram, ram_bytes: Some(8 * 1024 * 1024), duration: Duration::from_millis(400), sample_interval: Duration::from_millis(50), max_temp: 90.0, ..Default::default() }
    }

    #[test]
    fn ram_budget_is_clamped() {
        assert_eq!(plan_ram_bytes(1000, 0.5), 500);
        assert_eq!(plan_ram_bytes(1000, 2.0), 900);
        assert_eq!(plan_ram_bytes(1000, 0.0), 100);
    }

    #[test]
    fn pattern_fill_and_verify_detect_corruption() {
        let mut c = vec![0u64; 1000];
        let p = pattern_for(7);
        fill_chunk(&mut c, p);
        assert_eq!(count_errors(&c, p), 0);
        c[10] ^= 1 << 40;
        c[999] = 0;
        assert_eq!(count_errors(&c, p), 2);
        assert_ne!(pattern_for(1), pattern_for(2));
    }

    #[test]
    fn cpu_stress_runs_for_the_requested_time() {
        let mut probe = Fake { temp: Some(60.0), reads: 0 };
        let seen = std::cell::Cell::new(0);
        let r = run(&quick(true, false), &mut probe, &Cancel::new(), &|_| seen.set(seen.get() + 1));
        assert_eq!(r.reason, StopReason::Time);
        assert!(r.passed());
        assert!(r.samples.len() >= 3, "{}", r.samples.len());
        assert_eq!(seen.get(), r.samples.len());
        assert_eq!(r.max_temp, Some(60.0));
        assert_eq!(r.avg_load, Some(99.0));
        assert_eq!(r.avg_power, Some(65.0));
        assert!(r.seconds >= 0.4 && r.seconds < 5.0, "{}", r.seconds);
    }

    #[test]
    fn stops_when_the_temperature_limit_is_reached() {
        let mut probe = Fake { temp: Some(93.0), reads: 0 };
        let mut cfg = quick(true, false);
        cfg.duration = Duration::from_secs(30);
        let r = run(&cfg, &mut probe, &Cancel::new(), &|_| {});
        assert_eq!(r.reason, StopReason::Temperature(93.0));
        assert!(!r.passed());
        assert!(r.seconds < 5.0, "tem de parar na hora, não esperar os 30 s: {}", r.seconds);
        assert!(r.log.last().unwrap().contains("93"));
    }

    #[test]
    fn can_be_cancelled() {
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            c2.cancel();
        });
        let mut cfg = quick(true, false);
        cfg.duration = Duration::from_secs(30);
        let r = run(&cfg, &mut Fake { temp: None, reads: 0 }, &cancel, &|_| {});
        h.join().unwrap();
        assert_eq!(r.reason, StopReason::Cancelled);
        assert!(r.seconds < 5.0);
        assert_eq!(r.max_temp, None);
    }

    #[test]
    fn ram_stress_completes_passes_without_errors() {
        let mut cfg = quick(false, true);
        cfg.duration = Duration::from_millis(500);
        let r = run(&cfg, &mut Fake { temp: None, reads: 0 }, &Cancel::new(), &|_| {});
        assert_eq!(r.reason, StopReason::Time);
        assert_eq!(r.mem_errors, 0);
        assert_eq!(r.mem_bytes, 8 * 1024 * 1024);
        assert!(r.mem_passes >= 1, "passadas: {}", r.mem_passes);
        assert!(r.log.iter().any(|l| l.contains("RAM alocada")));
    }

    #[test]
    fn cpu_and_ram_together() {
        let mut cfg = quick(true, true);
        cfg.duration = Duration::from_millis(500);
        let r = run(&cfg, &mut Fake { temp: Some(70.0), reads: 0 }, &Cancel::new(), &|_| {});
        assert!(r.passed());
        assert!(r.mem_passes >= 1);
    }

    #[test]
    fn nothing_selected_is_a_failure() {
        let r = run(&quick(false, false), &mut Fake { temp: None, reads: 0 }, &Cancel::new(), &|_| {});
        assert!(matches!(r.reason, StopReason::Failed(_)));
    }

    #[test]
    fn stop_reason_texts() {
        assert!(StopReason::Temperature(91.4).text().contains("91"));
        assert!(StopReason::MemoryErrors(3).text().contains("3 erro"));
        assert_eq!(StopReason::Time.text(), "Tempo concluído");
    }
}
