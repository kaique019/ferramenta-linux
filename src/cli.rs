//! Linha de comando: cada subcomando coleta, mostra em texto e, com `--json`, em JSON (para scripts).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::bench::{disk, memory, render, sim};
use crate::util::{Cancel, Roots, fmt_bytes, fmt_duration};
use crate::{gpu, hwerrors, pci, sensors, storage, stress, sysinfo};

#[derive(Parser, Debug)]
#[command(name = "resetexplode", version, about = "RESET EXPLODE para Linux: informações, sensores, benchmarks e stress")]
pub struct Cli {
    /// Saída em JSON (para scripts)
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sistema, placa-mãe, CPU, memória e telas
    Info,
    /// Sensores de temperatura, ventoinhas, tensão e consumo
    Sensors,
    /// Placas de vídeo (NVIDIA, AMD, Intel)
    Gpu,
    /// Discos, saúde SMART e pontos de montagem
    Disks {
        /// Lê o SMART (precisa de smartctl; pede sudo se não for root)
        #[arg(long)]
        smart: bool,
    },
    /// Dispositivos PCI
    Pci,
    /// Erros de hardware no log do kernel
    Errors {
        /// Só ocorrências dos últimos N minutos
        #[arg(long)]
        minutes: Option<u64>,
    },
    /// Benchmarks
    Bench {
        #[command(subcommand)]
        which: Bench,
    },
    /// Teste de estabilidade (CPU e/ou RAM) com limite de temperatura
    Stress {
        /// Duração em segundos
        #[arg(long, default_value_t = 60)]
        secs: u64,
        /// Testa também a memória RAM
        #[arg(long)]
        ram: bool,
        /// Não carrega a CPU (só RAM)
        #[arg(long)]
        no_cpu: bool,
        /// Fração da RAM disponível a usar (0,1 a 0,9)
        #[arg(long, default_value_t = 0.6)]
        ram_fraction: f64,
        /// Para o teste ao atingir esta temperatura da CPU (°C)
        #[arg(long, default_value_t = 90.0)]
        max_temp: f64,
    },
}

#[derive(Subcommand, Debug)]
pub enum Bench {
    /// Renderização (path tracer na CPU)
    Render {
        /// Duração mínima em segundos (sempre termina ao menos uma passada completa, que pode levar mais)
        #[arg(long, default_value_t = 10)]
        secs: u64,
    },
    /// Simulação de jogo (física, colisão, IA)
    Sim {
        #[arg(long, default_value_t = 15)]
        secs: u64,
    },
    /// Memória: latência e largura de banda
    Memory {
        /// Tamanho do bloco de banda em MiB
        #[arg(long, default_value_t = 1024)]
        mb: usize,
    },
    /// Disco: sequencial e 4 KiB (E/S direta)
    Disk {
        /// Ponto de montagem (padrão: /tmp)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Tamanho do arquivo de teste em MiB
        #[arg(long, default_value_t = 256)]
        mb: u64,
        /// Segundos por teste aleatório
        #[arg(long, default_value_t = 3.0)]
        secs: f64,
    },
    /// Carregamento de mapa (pacote grande + arquivos pequenos)
    Load {
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Cancel acionado por Ctrl-C: o teste para limpo (apaga temporários) em vez de ser morto.
fn cancel_on_ctrl_c() -> Cancel {
    let c = Cancel::new();
    // SAFETY: o handler só grava em um AtomicBool, o que é seguro em contexto de sinal.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    let c2 = c.clone();
    std::thread::spawn(move || {
        while !c2.is_cancelled() {
            if INTERRUPTED.load(Ordering::SeqCst) {
                c2.cancel();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    c
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn opt<T: std::fmt::Display>(v: &Option<T>, unit: &str) -> String {
    v.as_ref().map(|x| format!("{x}{unit}")).unwrap_or_else(|| "—".into())
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

pub fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    let roots = Roots::system();
    match cli.command {
        None => bail!("a interface em terminal (TUI) ainda não está pronta; use um subcomando (veja --help)"),
        Some(Command::Info) => info(&roots, json),
        Some(Command::Sensors) => {
            let r = sensors::read_all(&roots);
            if json {
                return print_json(&r);
            }
            if r.is_empty() {
                println!("Nenhum sensor encontrado (máquina virtual ou sem drivers hwmon).");
            }
            let mut last = String::new();
            for s in &r {
                if s.chip_display != last {
                    println!("\n{}", s.chip_display);
                    last = s.chip_display.clone();
                }
                println!("  {:<24} {:>9.1}  {:?}", s.label, s.value, s.kind);
            }
            Ok(())
        }
        Some(Command::Gpu) => {
            let g = gpu::collect(&roots);
            if json {
                return print_json(&g);
            }
            if g.is_empty() {
                println!("Nenhuma GPU encontrada.");
            }
            for x in &g {
                println!("[{}] {} ({})", x.index, x.name, x.vendor.name());
                println!("    driver {} · VRAM {} / {} · temp {} · uso {} · consumo {}",
                    x.driver_version.as_deref().or(x.driver.as_deref()).unwrap_or("—"),
                    x.vram_used.map(fmt_bytes).unwrap_or("—".into()), x.vram_total.map(fmt_bytes).unwrap_or("—".into()),
                    opt(&x.temp, " °C"), opt(&x.util_gpu, " %"), opt(&x.power_w, " W"));
            }
            Ok(())
        }
        Some(Command::Disks { smart }) => {
            let d = storage::collect_disks(&roots, smart);
            let m = storage::collect_mounts(&roots);
            if json {
                #[derive(Serialize)]
                struct Out<'a> {
                    disks: &'a [storage::Disk],
                    mounts: &'a [storage::Mount],
                }
                return print_json(&Out { disks: &d, mounts: &m });
            }
            for x in &d {
                println!("{} — {} · {} · {} · {}", x.name, x.title(), fmt_bytes(x.size), x.kind(), x.bus);
                println!("    temp {} · veredito {:?}", opt(&x.temp, " °C"), x.verdict);
                for r in &x.reasons {
                    println!("    - {r}");
                }
                if let Some(n) = &x.smart_note {
                    println!("    SMART: {n}");
                }
            }
            println!();
            for x in &m {
                println!("{:<24} {:<8} {:>10} usados de {:>10}  ({})", x.mountpoint, x.fstype, fmt_bytes(x.used), fmt_bytes(x.total), x.source);
            }
            Ok(())
        }
        Some(Command::Pci) => {
            let p = pci::list(&roots);
            if json {
                return print_json(&p);
            }
            for x in &p {
                println!("{x:?}");
            }
            Ok(())
        }
        Some(Command::Errors { minutes }) => {
            let since = minutes.map(|m| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                now.saturating_sub(m * 60)
            });
            let e = hwerrors::errors_since(since);
            if json {
                return print_json(&e);
            }
            if let Some(err) = &e.error {
                println!("Não foi possível ler o log: {err}");
            }
            println!("CPU {} · memória {} · PCIe {} · GPU {} · disco {} · térmico {} · OOM {}", e.cpu, e.memory, e.pcie, e.gpu, e.storage, e.thermal, e.oom);
            if let Some(d) = &e.last_detail {
                println!("Última: {d}");
            }
            Ok(())
        }
        Some(Command::Bench { which }) => bench(which, json),
        Some(Command::Stress { secs, ram, no_cpu, ram_fraction, max_temp }) => {
            if no_cpu && !ram {
                bail!("nada a testar: use --ram junto com --no-cpu");
            }
            let cfg = stress::StressConfig { cpu: !no_cpu, ram, ram_fraction, max_temp, duration: Duration::from_secs(secs), ..Default::default() };
            let cancel = cancel_on_ctrl_c();
            let mut probe = stress::SystemProbe::new();
            let rep = stress::run(&cfg, &mut probe, &cancel, &|s| {
                if !json {
                    eprintln!("  {:>5.0}s  temp {:>7}  carga {:>7}  consumo {:>7}  erros RAM {}", s.t, opt(&s.cpu_temp.map(|v| format!("{v:.0}")), "°C"), opt(&s.cpu_load.map(|v| format!("{v:.0}")), "%"), opt(&s.power_w.map(|v| format!("{v:.0}")), "W"), s.mem_errors);
                }
            });
            if json {
                print_json(&rep)?;
            } else {
                println!("\nResultado: {:?} após {} — {}", rep.reason, fmt_duration(rep.seconds as u64), if rep.passed() { "APROVADO" } else { "NÃO APROVADO" });
                println!("Temp máx {} · carga média {} · consumo médio {} · RAM {} / {} passadas / {} erros", opt(&rep.max_temp.map(|v| format!("{v:.0}")), "°C"), opt(&rep.avg_load.map(|v| format!("{v:.0}")), "%"), opt(&rep.avg_power.map(|v| format!("{v:.0}")), "W"), fmt_bytes(rep.mem_bytes), rep.mem_passes, rep.mem_errors);
            }
            if rep.passed() { Ok(()) } else { std::process::exit(2) }
        }
    }
}

fn info(r: &Roots, json: bool) -> Result<()> {
    let os = sysinfo::collect_os(r);
    let board = sysinfo::collect_board(r);
    let cpu = sysinfo::collect_cpu(r);
    let mem = sysinfo::collect_mem(r, true);
    let displays = sysinfo::collect_displays(r);
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            os: &'a sysinfo::OsInfo,
            board: &'a sysinfo::BoardInfo,
            cpu: &'a sysinfo::CpuInfo,
            memory: &'a sysinfo::MemInfo,
            displays: &'a [sysinfo::DisplayInfo],
        }
        return print_json(&Out { os: &os, board: &board, cpu: &cpu, memory: &mem, displays: &displays });
    }
    println!("Sistema  {} · kernel {} · {}", os.pretty_name, os.kernel, os.arch);
    println!("         {} · ligado há {} · {}", os.hostname, fmt_duration(os.uptime_secs), os.virtualization.as_deref().map(|v| format!("virtualizado ({v})")).unwrap_or_else(|| "hardware real".into()));
    let placa = format!("{} {}", board.board_vendor, board.board_name).trim().to_string();
    let bios = format!("{} {}", board.bios_vendor, board.bios_version).trim().to_string();
    if !placa.is_empty() || !bios.is_empty() {
        println!("Placa    {} · BIOS {}", if placa.is_empty() { "—" } else { &placa }, if bios.is_empty() { "—" } else { &bios });
    }
    println!("CPU      {} · {} núcleos / {} threads", cpu.model, cpu.cores, cpu.threads);
    println!("Memória  {} total · {} disponível · swap {}", fmt_bytes(mem.total), fmt_bytes(mem.available), fmt_bytes(mem.swap_total));
    for m in &mem.modules {
        println!("         {m:?}");
    }
    for d in &displays {
        println!("Tela     {d:?}");
    }
    Ok(())
}

fn bench(which: Bench, json: bool) -> Result<()> {
    let cancel = cancel_on_ctrl_c();
    let bar = |label: &str, f: f64| {
        if !json {
            eprint!("\r  {label:<44} {:>3.0}%", f * 100.0);
        }
    };
    match which {
        Bench::Render { secs } => {
            let p = render::Progress::new(render::RenderParams::OFFICIAL);
            let res = render::run(threads(), Duration::from_secs(secs), &cancel, &p, &|_| {});
            if json {
                return print_json(&res);
            }
            println!("Render: nota {:.0} · {} threads · {} passadas · {:.1}s", res.score(), res.threads, res.passes, res.seconds);
        }
        Bench::Sim { secs } => {
            let p = sim::SimProgress::new();
            let res = sim::run(Duration::from_secs(secs), &cancel, &p);
            if json {
                return print_json(&res);
            }
            println!("Simulação: {:.0} FPS médio · 1% low {:.0} FPS · {} entidades · {} threads · serial {:.2} ms / paralelo {:.2} ms", res.avg_fps, res.low1_fps, res.entities, res.threads, res.main_ms, res.parallel_ms);
        }
        Bench::Memory { mb } => {
            let latency_ns = memory::measure_latency_ns(256 << 20, 4_000_000, &cancel);
            let (read_gbs, copy_gbs) = memory::measure_bandwidth(mb << 20, &cancel);
            let res = memory::MemoryResult { latency_ns, read_gbs, copy_gbs };
            if json {
                return print_json(&res);
            }
            println!("Memória: latência {:.1} ns · leitura {:.1} GB/s · cópia {:.1} GB/s", res.latency_ns, res.read_gbs, res.copy_gbs);
        }
        Bench::Disk { path, mb, secs } => {
            let dir = path.unwrap_or_else(|| PathBuf::from("/tmp"));
            let res = disk::run(&dir, mb, secs, &bar, &cancel).map_err(anyhow::Error::msg).context("benchmark de disco")?;
            if json {
                return print_json(&res);
            }
            eprintln!();
            println!("Disco {}: {}{}", res.target, res.summary(), if res.direct { "" } else { " (sem E/S direta: pode incluir cache)" });
        }
        Bench::Load { path } => {
            let dir = path.unwrap_or_else(|| PathBuf::from("/tmp"));
            let res = memory::measure_loading(&dir, 512 << 20, 1000, 64 << 10, &bar, &cancel).map_err(anyhow::Error::msg).context("teste de carregamento")?;
            if json {
                return print_json(&res);
            }
            eprintln!();
            println!("Carregamento: pacote {:.0} MB/s · arquivos pequenos {:.0} MB/s ({:.0}/s) · mapa em ~{:.1}s", res.seq_read_mbs, res.small_files_mbs, res.files_per_sec, res.map_load_seconds);
        }
    }
    if cancel.is_cancelled() {
        eprintln!("\nInterrompido.");
        std::process::exit(130);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_global_json_flag_anywhere() {
        let c = Cli::try_parse_from(["resetexplode", "info", "--json"]).unwrap();
        assert!(c.json && matches!(c.command, Some(Command::Info)));
        let c = Cli::try_parse_from(["resetexplode", "--json", "bench", "disk", "--mb", "64", "--secs", "0.5"]).unwrap();
        assert!(c.json);
        assert!(matches!(c.command, Some(Command::Bench { which: Bench::Disk { mb: 64, .. } })));
    }

    #[test]
    fn stress_defaults_are_safe() {
        let c = Cli::try_parse_from(["resetexplode", "stress"]).unwrap();
        let Some(Command::Stress { secs, ram, no_cpu, max_temp, .. }) = c.command else { panic!() };
        assert_eq!((secs, ram, no_cpu, max_temp), (60, false, false, 90.0));
    }

    #[test]
    fn rejects_unknown_subcommand_and_nothing_to_stress() {
        assert!(Cli::try_parse_from(["resetexplode", "explodir"]).is_err());
        let c = Cli::try_parse_from(["resetexplode", "stress", "--no-cpu"]).unwrap();
        assert!(run(c).is_err());
    }
}
