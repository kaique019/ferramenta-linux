//! Testes de carga e benchmarks executados em segundo plano, com progresso e cancelamento.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bench::{disk, memory, render, sim};
use crate::settings::{BenchRecord, BenchRun, OcRecord};
use crate::stress::{self, StressConfig};
use crate::tui::data::Env;
use crate::util::{Task, spawn_task};
use crate::{occheck, sysinfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Render,
    Sim,
    Memory,
    Disk,
    Load,
    Stress,
    Oc,
}

pub const ALL: [JobKind; 7] = [JobKind::Render, JobKind::Sim, JobKind::Memory, JobKind::Disk, JobKind::Load, JobKind::Stress, JobKind::Oc];

impl JobKind {
    pub fn title(self) -> &'static str {
        match self {
            JobKind::Render => "Renderização (CPU)",
            JobKind::Sim => "Simulação de jogo (CPU)",
            JobKind::Memory => "Memória: latência e banda",
            JobKind::Disk => "Disco: sequencial e 4 KiB",
            JobKind::Load => "Carregamento de mapa",
            JobKind::Stress => "Stress de CPU + RAM (60 s)",
            JobKind::Oc => "Estabilidade de overclock (10 min)",
        }
    }

    pub fn desc(self) -> &'static str {
        match self {
            JobKind::Render => "Path tracer em todos os núcleos. Mede desempenho de renderização e de CPU em carga pesada.",
            JobKind::Sim => "30 mil unidades com física, colisão e IA, como um jogo de estratégia. Mostra FPS médio e 1% low.",
            JobKind::Memory => "Latência de acesso aleatório e largura de banda de leitura e cópia da RAM.",
            JobKind::Disk => "Velocidade sequencial e de 4 KiB (E/S direta) na sua pasta pessoal. Cria e apaga arquivos temporários.",
            JobKind::Load => "Simula o carregamento de um mapa: um pacote grande mais mil arquivos pequenos.",
            JobKind::Stress => "Carrega todos os núcleos e testa a RAM; para sozinho se a CPU passar de 90 °C.",
            JobKind::Oc => "Para conferir overclock ou undervolt: estressa CPU e RAM e procura erros de hardware no log do kernel.",
        }
    }

    pub fn heavy(self) -> bool {
        matches!(self, JobKind::Render | JobKind::Sim | JobKind::Stress | JobKind::Oc)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Prog {
    pub label: String,
    pub fraction: f64,
}

pub enum Record {
    Run(BenchRun),
    Bench(BenchRecord),
    Oc(OcRecord),
}

pub struct Done {
    pub lines: Vec<String>,
    pub record: Option<Record>,
    pub ok: bool,
    /// O usuário cancelou: resultados parciais não valem e não são salvos.
    pub cancelled: bool,
}

pub type JobTask = Task<Prog, Result<Done, String>>;

pub struct Running {
    pub kind: JobKind,
    pub task: JobTask,
    pub render: Option<Arc<render::Progress>>,
    pub sim: Option<Arc<sim::SimProgress>>,
    pub last: Prog,
    pub started: Instant,
}

impl Running {
    /// Fração concluída (0 a 1): do progresso compartilhado quando existe, ou do último aviso da tarefa.
    pub fn fraction(&self) -> f64 {
        if let Some(r) = &self.render {
            r.fraction()
        } else if let Some(s) = &self.sim {
            s.fraction()
        } else {
            self.last.fraction
        }
    }

    pub fn label(&self) -> String {
        if let Some(s) = &self.sim {
            format!("{:.0} FPS agora", s.current_fps())
        } else if let Some(r) = &self.render {
            format!("{} de {} blocos", r.buckets_done.load(std::sync::atomic::Ordering::Relaxed), r.buckets_total.load(std::sync::atomic::Ordering::Relaxed))
        } else if self.last.label.is_empty() {
            "medindo...".into()
        } else {
            self.last.label.clone()
        }
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

fn now() -> u64 {
    crate::util::now_unix()
}

pub fn start(kind: JobKind, env: &Env) -> Running {
    let home = env.home.clone();
    let roots = env.roots.clone();
    let mut render_p = None;
    let mut sim_p = None;
    let task: JobTask = match kind {
        JobKind::Render => {
            let p = render::Progress::new(render::RenderParams::OFFICIAL);
            render_p = Some(p.clone());
            spawn_task(move |_rep, cancel| {
                let res = render::run(threads(), Duration::from_secs(10), cancel, &p, &|_| {});
                let cpu = sysinfo::collect_cpu(&roots).model;
                Ok(Done { lines: vec![format!("Nota: {:.0}", res.score()), format!("{} threads · {} passadas · {:.1} s", res.threads, res.passes, res.seconds)], record: keep(cancel, Record::Run(BenchRun { date: now(), multi: Some(res.score()), single: None, sustained: false, cpu })), ok: !cancel.is_cancelled(), cancelled: cancel.is_cancelled() })
            })
        }
        JobKind::Sim => {
            let p = sim::SimProgress::new();
            sim_p = Some(p.clone());
            spawn_task(move |_rep, cancel| {
                let r = sim::run(Duration::from_secs(15), cancel, &p);
                let summary = format!("{:.0} FPS · 1% low {:.0} · {} unidades · {} threads", r.avg_fps, r.low1_fps, r.entities, r.threads);
                Ok(Done { lines: vec![summary.clone(), format!("Parte serial {:.2} ms · parte paralela {:.2} ms por frame", r.main_ms, r.parallel_ms)], record: keep(cancel, Record::Bench(BenchRecord { date: now(), test: "sim".into(), score: r.score(), summary })), ok: !cancel.is_cancelled(), cancelled: cancel.is_cancelled() })
            })
        }
        JobKind::Memory => spawn_task(move |rep, cancel| {
            rep(Prog { label: "latência...".into(), fraction: 0.1 });
            let latency_ns = memory::measure_latency_ns(256 << 20, 4_000_000, cancel);
            rep(Prog { label: "largura de banda...".into(), fraction: 0.5 });
            let (read_gbs, copy_gbs) = memory::measure_bandwidth(1 << 30, cancel);
            let summary = format!("latência {latency_ns:.0} ns · leitura {read_gbs:.1} GB/s · cópia {copy_gbs:.1} GB/s");
            Ok(Done { lines: vec![summary.clone()], record: keep(cancel, Record::Bench(BenchRecord { date: now(), test: "mem".into(), score: read_gbs * 10.0, summary })), ok: !cancel.is_cancelled(), cancelled: cancel.is_cancelled() })
        }),
        JobKind::Disk => spawn_task(move |rep, cancel| {
            let r = disk::run(&home, 256, 3.0, &|l, f| rep(Prog { label: l.to_string(), fraction: f }), cancel)?;
            let mut lines = vec![r.summary(), format!("latência 4K: leitura {:.0} µs · escrita {:.0} µs", r.read_latency_us, r.write_latency_us)];
            if !r.direct {
                lines.push("Este sistema de arquivos não aceita E/S direta: o resultado pode incluir cache.".into());
            }
            Ok(Done { lines, record: keep(cancel, Record::Bench(BenchRecord { date: now(), test: "disk".into(), score: r.score(), summary: r.summary() })), ok: !cancel.is_cancelled(), cancelled: cancel.is_cancelled() })
        }),
        JobKind::Load => spawn_task(move |rep, cancel| {
            let r = memory::measure_loading(&home, 512 << 20, 1000, 64 << 10, &|l, f| rep(Prog { label: l.to_string(), fraction: f }), cancel)?;
            let summary = format!("pacote {:.0} MB/s · arquivos pequenos {:.0} MB/s ({:.0}/s) · mapa em ~{:.1} s", r.seq_read_mbs, r.small_files_mbs, r.files_per_sec, r.map_load_seconds);
            Ok(Done { lines: vec![summary.clone()], record: keep(cancel, Record::Bench(BenchRecord { date: now(), test: "load".into(), score: 100.0 / r.map_load_seconds.max(0.01), summary })), ok: !cancel.is_cancelled(), cancelled: cancel.is_cancelled() })
        }),
        JobKind::Stress => spawn_task(move |rep, cancel| {
            let cfg = StressConfig { cpu: true, ram: true, duration: Duration::from_secs(60), ..Default::default() };
            let total = cfg.duration.as_secs_f64();
            let mut probe = stress::SystemProbe::new();
            let r = stress::run(&cfg, &mut probe, cancel, &|s| rep(Prog { label: sample_label(s), fraction: s.t / total }));
            let mut lines = vec![format!("{} — {}", r.reason.text(), if r.passed() { "APROVADO" } else { "NÃO APROVADO" })];
            lines.push(format!("temp máx {} · carga média {} · RAM {} passadas, {} erros", fmt(r.max_temp, "°C"), fmt(r.avg_load, "%"), r.mem_passes, r.mem_errors));
            Ok(Done { ok: r.passed(), cancelled: cancel.is_cancelled(), lines, record: None })
        }),
        JobKind::Oc => spawn_task(move |rep, cancel| {
            let cfg = StressConfig { cpu: true, ram: true, duration: Duration::from_secs(600), ..Default::default() };
            let total = cfg.duration.as_secs_f64();
            let o = occheck::run(&cfg, cancel, &|s| rep(Prog { label: sample_label(s), fraction: s.t / total }));
            let ok = matches!(o.verdict, occheck::OcVerdict::Stable | occheck::OcVerdict::StableWithWarnings);
            Ok(Done { lines: vec![format!("{} — {}", o.verdict.label(), o.summary)], record: Some(Record::Oc(o.record())), ok, cancelled: cancel.is_cancelled() })
        }),
    };
    Running { kind, task, render: render_p, sim: sim_p, last: Prog::default(), started: Instant::now() }
}

fn keep(cancel: &crate::util::Cancel, r: Record) -> Option<Record> {
    if cancel.is_cancelled() { None } else { Some(r) }
}

fn fmt(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.0}{unit}")).unwrap_or_else(|| "—".into())
}

fn sample_label(s: &stress::StressSample) -> String {
    format!("{:.0}s · temp {} · carga {} · erros de RAM {}", s.t, fmt(s.cpu_temp, "°C"), fmt(s.cpu_load, "%"), s.mem_errors)
}
