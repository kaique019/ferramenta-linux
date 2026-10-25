//! Linha de comando: cada subcomando coleta, mostra em texto e, com `--json`, em JSON (para scripts).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::bench::{disk, memory, render, sim};
use crate::util::{Cancel, Roots, fmt_bytes, fmt_duration};
use crate::settings::{OverlayConfig, SettingsStore};
use crate::{autostart, games, gpu, launch, report, hwerrors, installer, network, occheck, overlay, pci, power, sensors, serial, storage, stress, sysinfo, tweaks};

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
    /// Bateria, governador da CPU, boost e perfil de energia
    Power {
        /// Define o governador (performance, powersave...) — precisa de sudo
        #[arg(long)]
        governor: Option<String>,
        /// Define a preferência energia/desempenho (EPP)
        #[arg(long)]
        epp: Option<String>,
        /// Define o perfil de plataforma (low-power, balanced, performance)
        #[arg(long)]
        profile: Option<String>,
        /// Liga (on) ou desliga (off) o turbo
        #[arg(long, value_parser = ["on", "off"])]
        boost: Option<String>,
    },
    /// Rede: interfaces, DNS, latência, velocidade e bufferbloat
    Net {
        /// Também mede latência, velocidade de download e bufferbloat (usa a internet)
        #[arg(long)]
        test: bool,
        /// Host para a latência
        #[arg(long, default_value = "1.1.1.1")]
        host: String,
        /// URL de download para o teste de velocidade
        #[arg(long, default_value = "https://speed.cloudflare.com/__down?bytes=200000000")]
        url: String,
        /// Duração do teste de velocidade em segundos
        #[arg(long, default_value_t = 8.0)]
        secs: f64,
    },
    /// Erros de hardware no log do kernel
    Errors {
        /// Só ocorrências dos últimos N minutos
        #[arg(long)]
        minutes: Option<u64>,
    },
    /// Jogos instalados (Steam, Heroic/Epic/GOG, Lutris e manuais) e seus perfis
    Games {
        #[command(subcommand)]
        action: GameAction,
    },
    /// Relatório do sistema em HTML (ou JSON com --json)
    Report {
        /// Arquivo de saída (padrão: ~/resetexplode-AAAAMMDD.html)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Inclui o SMART dos discos (precisa de sudo)
        #[arg(long)]
        smart: bool,
    },
    /// Tema de cores da TUI (arquivo theme.json): mostra, lista presets ou grava um modelo para editar
    Theme {
        /// Lista os presets
        #[arg(long)]
        list: bool,
        /// Preset a mostrar ou gravar (padrao, claro, contraste, retro, neon)
        #[arg(long)]
        preset: Option<String>,
        /// Grava o tema completo em theme.json para você editar
        #[arg(long)]
        write: bool,
        /// Sobrescreve um theme.json existente (guarda .bak)
        #[arg(long)]
        force: bool,
    },
    /// Programas de inicialização (desativar com backup e restaurar)
    Startup {
        #[command(subcommand)]
        action: StartupAction,
    },
    /// Ajustes de sistema para jogos (com restauração)
    Tweaks {
        #[command(subcommand)]
        action: TweakAction,
    },
    /// Portas seriais USB e diagnóstico (dialout, brltty, ModemManager)
    Serial,
    /// Instala utilitários (MangoHud, GameMode, lm-sensors...) pelo gerenciador da distro
    Install {
        /// Programa do catálogo; sem isso, lista o catálogo
        id: Option<String>,
    },
    /// Overlay de FPS (MangoHud): mostra ou grava a configuração
    Overlay {
        /// Canto: TL, TR, BL ou BR
        #[arg(long, value_parser = ["TL", "TR", "BL", "BR"])]
        corner: Option<String>,
        /// Escala do texto (0,5 a 3)
        #[arg(long)]
        scale: Option<f64>,
        /// Itens separados por vírgula (ex.: fps,cpu_temp,gpu_load)
        #[arg(long)]
        items: Option<String>,
        /// Grava em ~/.config/MangoHud/MangoHud.conf
        #[arg(long)]
        write: bool,
    },
    /// Verificador de estabilidade (overclock/undervolt): estressa CPU+RAM e confere erros do kernel
    Oc {
        /// Duração em minutos
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        #[arg(long, default_value_t = 90.0)]
        max_temp: f64,
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
pub enum GameAction {
    /// Lista os jogos encontrados
    List,
    /// Mostra o perfil de um jogo
    Profile { id: String },
    /// Altera o perfil (só o que for informado)
    Set {
        id: String,
        #[arg(long, value_parser = ["normal", "above", "high"])]
        priority: Option<String>,
        #[arg(long)]
        gamemode: Option<bool>,
        #[arg(long)]
        mangohud: Option<bool>,
        #[arg(long)]
        dgpu: Option<bool>,
        #[arg(long)]
        performance_governor: Option<bool>,
        #[arg(long, value_parser = ["source", "unity", "unreal"])]
        style: Option<String>,
        /// default, 1280x720, 1920x1080, 2560x1440...
        #[arg(long)]
        res: Option<String>,
        #[arg(long, value_parser = ["default", "full", "borderless", "window"])]
        win_mode: Option<String>,
        #[arg(long)]
        hz: Option<u32>,
        /// Argumentos extras do jogo
        #[arg(long, allow_hyphen_values = true)]
        extra: Option<String>,
        /// Variáveis de ambiente (CHAVE=valor; separe várias com ';')
        #[arg(long)]
        env: Option<String>,
    },
    /// Executa o jogo com o perfil (Steam: mostra as opções de inicialização para colar)
    Launch { id: String },
    /// Adiciona um jogo manual
    Add {
        name: String,
        exe: String,
        /// Argumentos fixos do jogo
        #[arg(long, default_value = "", allow_hyphen_values = true)]
        args: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum StartupAction {
    /// Lista os programas que iniciam com a sessão
    List,
    /// Desativa (guarda backup)
    Disable { file: String },
    /// Lista os desativados por aqui
    Removed,
    /// Restaura um desativado (nome ou arquivo); sem argumento, restaura todos
    Restore { name: Option<String> },
}

#[derive(Subcommand, Debug)]
pub enum TweakAction {
    /// Mostra o estado de cada ajuste
    List,
    /// Aplica o valor recomendado (vale até reiniciar)
    Apply { id: String },
    /// Devolve o valor original
    Restore { id: String },
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
pub fn cancel_on_ctrl_c() -> Cancel {
    INTERRUPTED.store(false, Ordering::SeqCst);
    let c = Cancel::new();
    // SAFETY: o handler só grava em um AtomicBool, o que é seguro em contexto de sinal.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    let c2 = c.clone();
    std::thread::spawn(move || {
        while !c2.is_cancelled() && !c2.is_orphaned() {
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
        None => crate::tui::run().map_err(|e| anyhow::anyhow!("{e}")),
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
        Some(Command::Power { governor, epp, profile, boost }) => {
            let mut changed = vec![];
            if let Some(g) = governor {
                let n = power::set_governor(&roots, &g).map_err(anyhow::Error::msg)?;
                changed.push(format!("governador {g} em {n} CPUs"));
            }
            if let Some(e) = epp {
                let n = power::set_epp(&roots, &e).map_err(anyhow::Error::msg)?;
                changed.push(format!("EPP {e} em {n} CPUs"));
            }
            if let Some(p) = profile {
                power::set_platform_profile(&roots, &p).map_err(anyhow::Error::msg)?;
                changed.push(format!("perfil {p}"));
            }
            if let Some(b) = boost {
                power::set_boost(&roots, b == "on").map_err(anyhow::Error::msg)?;
                changed.push(format!("boost {b}"));
            }
            let p = power::collect(&roots);
            if json {
                return print_json(&p);
            }
            for c in &changed {
                println!("Alterado: {c}");
            }
            println!("Tomada: {}", match p.on_ac { Some(true) => "ligada", Some(false) => "desligada (na bateria)", None => "—" });
            if p.batteries.is_empty() {
                println!("Sem bateria (desktop).");
            }
            for b in &p.batteries {
                println!("{}: {} · {} · saúde {} · {} ciclos · {}{}", b.name, opt(&b.capacity.map(|v| format!("{v:.0}")), " %"), b.status, opt(&b.health.map(|v| format!("{v:.0}")), " %"), opt(&b.cycles, ""), opt(&b.power_w.map(|v| format!("{v:.1}")), " W"), b.minutes_left.map(|m| format!(" · ~{}", fmt_duration(m * 60))).unwrap_or_default());
            }
            let c = &p.cpu;
            println!("CPU: governador {} ({}) · EPP {} · boost {} · perfil {}", opt(&c.governor, ""), c.governors.join("/"), opt(&c.epp, ""), c.boost.map(|b| if b { "ligado" } else { "desligado" }).unwrap_or("—"), opt(&c.platform_profile, ""));
            Ok(())
        }
        Some(Command::Games { action }) => games_cmd(&roots, action, json),
        Some(Command::Report { out, smart }) => {
            let st = SettingsStore::default_location().load().settings;
            let rep = report::collect(&roots, &st, smart);
            if json {
                return print_json(&rep);
            }
            let path = out.unwrap_or_else(|| crate::util::home_dir().join(format!("resetexplode-{}.html", crate::util::now_local("%Y%m%d"))));
            crate::util::write_atomic(&path, report::to_html(&rep).as_bytes()).with_context(|| format!("não consegui gravar {}", path.display()))?;
            crate::util::give_to_user(&path);
            println!("Relatório gravado em {}", path.display());
            Ok(())
        }
        Some(Command::Theme { list, preset, write, force }) => theme_cmd(list, preset, write, force),
        Some(Command::Startup { action }) => startup(action, json),
        Some(Command::Tweaks { action }) => tweak_cmd(&roots, action, json),
        Some(Command::Serial) => {
            let ports = serial::list(&roots);
            let advice = serial::diagnose(&ports, &serial::gather_facts());
            if json {
                #[derive(Serialize)]
                struct Out<'a> {
                    ports: &'a [serial::SerialPort],
                    advice: &'a [serial::Advice],
                }
                return print_json(&Out { ports: &ports, advice: &advice });
            }
            for p in &ports {
                println!("{} · {} {} · {}{}", p.device, opt(&p.vendor_id, ""), opt(&p.product_id, ""), p.product.as_deref().unwrap_or("?"), if p.accessible { "" } else { " · SEM ACESSO" });
            }
            for a in &advice {
                println!("\n! {}\n  → {}", a.problem, a.fix);
            }
            Ok(())
        }
        Some(Command::Install { id: None }) => {
            let c = installer::catalog();
            if json {
                return print_json(&c);
            }
            println!("Gerenciador: {}", installer::detect(&roots).map(|p| p.name()).unwrap_or("não reconhecido"));
            for a in c {
                println!("{:<14} {} {}", a.id, if a.installed { "[instalado]" } else { "[ausente]  " }, a.title);
            }
            Ok(())
        }
        Some(Command::Install { id: Some(id) }) => {
            installer::install(&roots, &id).map_err(anyhow::Error::msg)?;
            println!("{id} instalado.");
            Ok(())
        }
        Some(Command::Overlay { corner, scale, items, write }) => {
            let store = SettingsStore::default_location();
            let mut cfg = store.load().settings.overlay;
            if let Some(c) = corner {
                cfg.corner = c;
            }
            if let Some(sc) = scale {
                cfg.scale = sc;
            }
            if let Some(i) = items {
                let list: Vec<String> = i.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                if let Some(bad) = list.iter().find(|k| !overlay::METRICS.iter().any(|(key, _, _)| key == k)) {
                    bail!("item desconhecido: {bad} (opções: {})", overlay::METRICS.iter().map(|(k, _, _)| *k).collect::<Vec<_>>().join(", "));
                }
                cfg.items = Some(list);
            }
            save_overlay(&store, &cfg)?;
            if write {
                let path = overlay::default_path();
                overlay::write(&cfg, &path).map_err(anyhow::Error::msg)?;
                println!("Gravado em {}", path.display());
            } else {
                print!("{}", overlay::generate(&cfg));
            }
            Ok(())
        }
        Some(Command::Oc { minutes, max_temp }) => {
            if !(minutes > 0.0 && minutes <= 240.0) {
                bail!("--minutes deve ficar entre 0 e 240");
            }
            let cancel = cancel_on_ctrl_c();
            let cfg = stress::StressConfig { cpu: true, ram: true, max_temp, duration: Duration::from_secs_f64(minutes * 60.0), ..Default::default() };
            let o = occheck::run(&cfg, &cancel, &|s| {
                if !json {
                    eprintln!("  {:>5.0}s  temp {:>7}  carga {:>7}  erros RAM {}", s.t, opt(&s.cpu_temp.map(|v| format!("{v:.0}")), "°C"), opt(&s.cpu_load.map(|v| format!("{v:.0}")), "%"), s.mem_errors);
                }
            });
            let _ = SettingsStore::default_location().update(|st| st.push_oc_record(o.record()));
            if json {
                #[derive(Serialize)]
                struct Out<'a> {
                    verdict: &'a str,
                    summary: &'a str,
                    report: &'a stress::StressReport,
                    errors: &'a hwerrors::HardwareErrors,
                }
                print_json(&Out { verdict: o.verdict.label(), summary: &o.summary, report: &o.report, errors: &o.errors })?;
            } else {
                println!("\nVeredito: {} — {}", o.verdict.label(), o.summary);
            }
            if matches!(o.verdict, occheck::OcVerdict::Stable | occheck::OcVerdict::StableWithWarnings) { Ok(()) } else { std::process::exit(2) }
        }
        Some(Command::Net { test, host, url, secs }) => net(&roots, json, test, &host, &url, secs),
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

fn theme_cmd(list: bool, preset: Option<String>, write: bool, force: bool) -> Result<()> {
    use crate::tui::theme;
    if list {
        for p in theme::PRESETS {
            println!("{p}");
        }
        return Ok(());
    }
    let path = crate::util::app_config_dir().join("theme.json");
    let th = match &preset {
        Some(p) => theme::preset(p).with_context(|| format!("preset desconhecido: {p} (opções: {})", theme::PRESETS.join(", ")))?,
        None => {
            let (t, warn) = theme::load(&path);
            if let Some(w) = warn {
                eprintln!("aviso: {w}");
            }
            t
        }
    };
    let json = serde_json::to_string_pretty(&th.to_file())? + "\n";
    if !write {
        eprintln!("Arquivo do tema: {} ({})", path.display(), if path.exists() { "existe" } else { "não existe; usando o padrão" });
        print!("{json}");
        return Ok(());
    }
    if path.exists() {
        if !force {
            bail!("{} já existe; use --force para sobrescrever (a versão atual vira .bak)", path.display());
        }
        let bak = path.with_extension("json.bak");
        std::fs::copy(&path, &bak).with_context(|| format!("não consegui guardar {}", bak.display()))?;
        crate::util::give_to_user(&bak);
    }
    if let Some(dir) = path.parent() {
        crate::util::create_dir_for_user(dir).with_context(|| format!("não consegui criar {}", dir.display()))?;
    }
    crate::util::write_atomic(&path, json.as_bytes()).with_context(|| format!("não consegui gravar {}", path.display()))?;
    crate::util::give_to_user(&path);
    println!("Tema {} gravado em {}. Edite as cores e aperte T na TUI para recarregar.", th.name, path.display());
    Ok(())
}

fn save_overlay(store: &SettingsStore, cfg: &OverlayConfig) -> Result<()> {
    store.update(|s| s.overlay = cfg.clone()).context("não consegui salvar as configurações")
}

/// "discord", "Discord" e "discord.desktop" apontam para o mesmo item.
fn backup_matches(b: &crate::settings::StartupBackup, wanted: &str) -> bool {
    let w = wanted.to_lowercase();
    let w = w.strip_suffix(".desktop").unwrap_or(&w);
    let file = b.original.rsplit('/').next().unwrap_or("").to_lowercase();
    b.name.to_lowercase() == w || file.strip_suffix(".desktop").unwrap_or(&file) == w
}

fn games_cmd(roots: &Roots, action: GameAction, json: bool) -> Result<()> {
    let store = SettingsStore::default_location();
    let st = store.load().settings;
    let all = games::detect_all(&crate::util::home_dir(), &st.custom_games);
    let find = |id: &str| -> Result<&games::Game> {
        let by_id: Vec<_> = all.iter().filter(|g| g.id == id).collect();
        if let [g] = by_id[..] {
            return Ok(g);
        }
        if id.chars().count() < 3 {
            bail!("jogo não encontrado: {id} (nomes parciais precisam de ao menos 3 letras; veja `games list`)");
        }
        let by_name: Vec<_> = all.iter().filter(|g| g.name.to_lowercase().contains(&id.to_lowercase())).collect();
        match by_name[..] {
            [g] => Ok(g),
            [] => bail!("jogo não encontrado: {id} (veja `games list`)"),
            _ => bail!("\"{id}\" casa com {} jogos; use o id exato (veja `games list`)", by_name.len()),
        }
    };
    match action {
        GameAction::List => {
            if json {
                return print_json(&all);
            }
            if all.is_empty() {
                println!("Nenhum jogo encontrado (Steam, Heroic, Lutris). Adicione um com `games add`.");
            }
            for g in &all {
                println!("{:<28} {:<14} {}", g.id, g.source.name(), g.name);
            }
        }
        GameAction::Profile { id } => {
            let g = find(&id)?;
            let p = st.game_profiles.get(&g.id).cloned().unwrap_or_default();
            print_json(&p)?;
        }
        GameAction::Set { id, priority, gamemode, mangohud, dgpu, performance_governor, style, res, win_mode, hz, extra, env } => {
            let g = find(&id)?.clone();
            let mut p = st.game_profiles.get(&g.id).cloned().unwrap_or_default();
            if let Some(v) = priority { p.priority = v }
            if let Some(v) = gamemode { p.gamemode = v }
            if let Some(v) = mangohud { p.mangohud = v }
            if let Some(v) = dgpu { p.dgpu = v }
            if let Some(v) = performance_governor { p.performance_governor = v }
            if let Some(v) = style { p.style = v }
            if let Some(v) = res { p.res = v }
            if let Some(v) = win_mode { p.win_mode = v }
            if let Some(v) = hz { p.hz = v }
            if let Some(v) = extra { p.extra = v }
            if let Some(v) = env { p.env = v.split(';').map(str::trim).collect::<Vec<_>>().join("\n") }
            // valida antes de gravar: um perfil quebrado nunca é salvo
            launch::plan(&p, None, true, &|_| true).map_err(anyhow::Error::msg)?;
            store.update(|s| s.game_profiles.insert(g.id.clone(), p)).context("não consegui salvar o perfil")?;
            println!("Perfil de {} salvo.", g.name);
        }
        GameAction::Launch { id } => {
            let g = find(&id)?;
            let p = st.game_profiles.get(&g.id).cloned().unwrap_or_default();
            let dgpu = gpu::primary(&gpu::collect(roots)).map(|x| x.vendor);
            if let Some(appid) = g.steam_appid {
                let pl = launch::plan(&p, dgpu, crate::util::is_root(), &crate::util::have).map_err(anyhow::Error::msg)?;
                println!("Steam não aceita opções de fora. Cole isto em Propriedades → Opções de inicialização de \"{}\" (appid {appid}):\n\n  {}\n", g.name, launch::steam_options(&pl));
                for w in &pl.warnings {
                    eprintln!("aviso: {w}");
                }
                return Ok(());
            }
            let exe = p.exe_path.clone().or_else(|| g.exe.clone()).context("este jogo não tem executável conhecido (defina com `games add`)")?;
            let base = st.custom_games.iter().find(|c| c.exe == exe).map(|c| c.args.clone()).unwrap_or_default();
            let cancel = cancel_on_ctrl_c();
            let o = launch::run(&p, &exe, &base, dgpu, &cancel).map_err(anyhow::Error::msg)?;
            for w in &o.warnings {
                eprintln!("aviso: {w}");
            }
            if o.cancelled {
                std::process::exit(130);
            }
            println!("{} terminou (código {}).", g.name, o.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()));
        }
        GameAction::Add { name, exe, args } => {
            if !std::path::Path::new(&exe).is_file() {
                bail!("{exe} não é um arquivo");
            }
            launch::parse_args(&args).map_err(anyhow::Error::msg)?;
            store.update(|s| s.custom_games.push(crate::settings::CustomGame { name: name.clone(), exe, args })).context("não consegui salvar")?;
            println!("{name} adicionado.");
        }
    }
    Ok(())
}

fn startup(action: StartupAction, json: bool) -> Result<()> {
    let dirs = autostart::Dirs::system_default();
    let store = SettingsStore::default_location();
    match action {
        StartupAction::List => {
            let l = autostart::list(&dirs);
            if json {
                return print_json(&l);
            }
            for e in &l {
                println!("{:<34} {:<9} {} {}", e.file, if e.enabled { "ativo" } else { "desativado" }, if e.system { "(sistema)" } else { "" }, e.exec);
            }
        }
        StartupAction::Disable { file } => {
            let b = autostart::disable(&dirs, &file).map_err(anyhow::Error::msg)?;
            store.update(|s| s.startup_removed.push(b.clone())).context("não consegui salvar o registro de backup")?;
            println!("Desativado: {} (backup guardado; restaure com `startup restore`)", b.name);
        }
        StartupAction::Removed => {
            let r = store.load().settings.startup_removed;
            if json {
                return print_json(&r);
            }
            for b in &r {
                println!("{} — {}", b.name, b.original);
            }
        }
        StartupAction::Restore { name } => {
            let all = store.load().settings.startup_removed;
            let wanted: Vec<_> = all.iter().filter(|b| name.as_ref().is_none_or(|n| backup_matches(b, n))).cloned().collect();
            if wanted.is_empty() {
                bail!("nada para restaurar{}", name.map(|n| format!(" com o nome {n}")).unwrap_or_default());
            }
            let mut ok = vec![];
            for b in &wanted {
                match autostart::restore(b) {
                    Ok(()) => {
                        println!("Restaurado: {}", b.name);
                        ok.push(b.original.clone());
                    }
                    Err(e) => eprintln!("Falhou ({}): {e}", b.name),
                }
            }
            store.update(|s| s.startup_removed.retain(|b| !ok.contains(&b.original))).context("não consegui atualizar o registro")?;
            if ok.len() < wanted.len() {
                bail!("{} item(ns) não foram restaurados", wanted.len() - ok.len());
            }
        }
    }
    Ok(())
}

fn tweak_cmd(roots: &Roots, action: TweakAction, json: bool) -> Result<()> {
    let store = SettingsStore::default_location();
    match action {
        TweakAction::List => {
            let st = tweaks::status(roots, &store.load().settings.tweak_backup);
            if json {
                return print_json(&st);
            }
            for t in &st {
                println!("{:<20} atual {:<10} recomendado {:<8} {}{}", t.id, t.current.as_deref().unwrap_or("—"), t.recommended, if t.applied { "[aplicado]" } else if !t.available { "[indisponível]" } else { "" }, if t.can_restore { " [restaurável]" } else { "" });
            }
        }
        TweakAction::Apply { id } => {
            let res = store.update(|s| tweaks::apply(roots, &mut s.tweak_backup, &id)).context("não consegui salvar o backup")?;
            res.map_err(anyhow::Error::msg)?;
            println!("{id} aplicado (vale até reiniciar; desfaça com `tweaks restore {id}`).");
        }
        TweakAction::Restore { id } => {
            let res = store.update(|s| tweaks::restore(roots, &mut s.tweak_backup, &id)).context("não consegui salvar as configurações")?;
            res.map_err(anyhow::Error::msg)?;
            println!("{id} restaurado.");
        }
    }
    Ok(())
}

fn net(r: &Roots, json: bool, test: bool, host: &str, url: &str, secs: f64) -> Result<()> {
    #[derive(Serialize)]
    struct Out {
        interfaces: Vec<network::Interface>,
        dns_servers: Vec<String>,
        dns_ms: Option<f64>,
        latency: Option<network::LatencyStats>,
        bufferbloat: Option<network::Bufferbloat>,
    }
    let cancel = cancel_on_ctrl_c();
    let mut out = Out { interfaces: network::interfaces(r), dns_servers: network::dns_servers(r), dns_ms: None, latency: None, bufferbloat: None };
    if test {
        out.dns_ms = network::dns_time("example.com");
        out.latency = Some(network::tcp_latency(host, 443, 10, Duration::from_secs(2), &cancel));
        out.bufferbloat = Some(network::bufferbloat(host, 443, url, secs, &cancel));
    }
    if json {
        return print_json(&out);
    }
    for i in &out.interfaces {
        println!("{:<10} {:<9} {} {} {}", i.name, i.kind, if i.up { "ativa " } else { "inativa" }, opt(&i.speed_mbps, " Mb/s"), i.driver.as_deref().unwrap_or(""));
    }
    println!("DNS: {}", if out.dns_servers.is_empty() { "—".into() } else { out.dns_servers.join(", ") });
    if let Some(l) = &out.latency {
        println!("Latência {}: mediana {} · p95 {} · jitter {} · perda {:.0}%", l.target, opt(&l.median_ms.map(|v| format!("{v:.1}")), " ms"), opt(&l.p95_ms.map(|v| format!("{v:.1}")), " ms"), opt(&l.jitter_ms.map(|v| format!("{v:.1}")), " ms"), l.loss_pct);
        println!("Resolução de DNS: {}", opt(&out.dns_ms.map(|v| format!("{v:.0}")), " ms"));
    }
    if let Some(b) = &out.bufferbloat {
        match &b.download {
            Some(d) => println!("Download: {:.0} Mb/s", d.mbps),
            None => println!("Download: falhou (sem internet ou URL bloqueada)"),
        }
        println!("Bufferbloat: nota {} (latência sobe {} sob carga)", b.grade, opt(&b.added_ms.map(|v| format!("{v:.0}")), " ms"));
    }
    Ok(())
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
    fn parses_power_and_net() {
        let c = Cli::try_parse_from(["resetexplode", "power", "--governor", "performance", "--boost", "off"]).unwrap();
        assert!(matches!(c.command, Some(Command::Power { governor: Some(_), boost: Some(_), .. })));
        assert!(Cli::try_parse_from(["resetexplode", "power", "--boost", "talvez"]).is_err());
        let c = Cli::try_parse_from(["resetexplode", "net", "--test"]).unwrap();
        assert!(matches!(c.command, Some(Command::Net { test: true, .. })));
    }

    #[test]
    fn restore_name_matching_is_forgiving_but_exact() {
        let b = crate::settings::StartupBackup { name: "Discord".into(), original: "/home/u/.config/autostart/discord-ptb.desktop".into(), backup: String::new(), removed_at: 0 };
        assert!(backup_matches(&b, "discord") && backup_matches(&b, "DISCORD") && backup_matches(&b, "discord-ptb.desktop") && backup_matches(&b, "discord-ptb"));
        assert!(!backup_matches(&b, "disc") && !backup_matches(&b, "ptb"));
    }

    #[test]
    fn game_args_may_start_with_a_hyphen() {
        let c = Cli::try_parse_from(["resetexplode", "games", "add", "Jogo", "/bin/true", "--args", "--base -x"]).unwrap();
        assert!(matches!(c.command, Some(Command::Games { action: GameAction::Add { ref args, .. } }) if args == "--base -x"));
        let c = Cli::try_parse_from(["resetexplode", "games", "set", "portal", "--extra", "-novid -console", "--gamemode", "true"]).unwrap();
        assert!(matches!(c.command, Some(Command::Games { action: GameAction::Set { gamemode: Some(true), .. } })));
    }

    #[test]
    fn parses_theme_command() {
        let c = Cli::try_parse_from(["resetexplode", "theme", "--preset", "retro", "--write", "--force"]).unwrap();
        assert!(matches!(c.command, Some(Command::Theme { write: true, force: true, preset: Some(_), .. })));
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
