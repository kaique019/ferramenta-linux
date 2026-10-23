//! Informações do sistema: SO, placa-mãe/BIOS (DMI), CPU, memória e monitores.
//! Tudo vem de /proc, /sys e /etc (via [`Roots`], para poder testar com árvores falsas);
//! os módulos de RAM dependem do `dmidecode`, que exige privilégios.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::Duration;

use serde::Serialize;

use crate::util::*;

// ---------------------------------------------------------------------------------------------
// Sistema operacional
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct OsInfo {
    pub pretty_name: String,
    pub id: String,
    pub kernel: String,
    pub arch: String,
    pub hostname: String,
    pub uptime_secs: u64,
    pub load_avg: Option<(f64, f64, f64)>,
    pub session_type: Option<String>,
    pub desktop: Option<String>,
    pub init: String,
    pub boot_mode: String,
    pub secure_boot: Option<bool>,
    pub virtualization: Option<String>,
}

pub fn collect_os(r: &Roots) -> OsInfo {
    let osr = parse_env_file(&fs::read_to_string(r.etc.join("os-release")).unwrap_or_default());
    let get = |k: &str| osr.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    let uptime = read_trim(r.proc.join("uptime"))
        .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()))
        .unwrap_or(0.0);
    let load_avg = read_trim(r.proc.join("loadavg")).and_then(|s| {
        let mut it = s.split_whitespace().filter_map(|x| x.parse::<f64>().ok());
        Some((it.next()?, it.next()?, it.next()?))
    });
    let efi = r.sys.join("firmware/efi");
    let secure_boot = fs::read_dir(efi.join("efivars")).ok().and_then(|rd| {
        rd.filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("SecureBoot-"))
            .and_then(|e| fs::read(e.path()).ok())
            .and_then(|b| b.last().map(|v| *v == 1))
    });
    let hypervisor = fs::read_to_string(r.proc.join("cpuinfo")).map(|t| t.contains(" hypervisor")).unwrap_or(false);
    let virtualization = if r.proc == std::path::Path::new("/proc") {
        run("systemd-detect-virt", &[], Duration::from_secs(2))
            .filter(CmdOut::ok)
            .map(|o| o.stdout.trim().to_string())
            .filter(|s| !s.is_empty() && s != "none")
            .or_else(|| hypervisor.then(|| "máquina virtual".to_string()))
    } else {
        hypervisor.then(|| "máquina virtual".to_string())
    };
    OsInfo {
        pretty_name: get("PRETTY_NAME").or_else(|| get("NAME")).unwrap_or_else(|| "Linux".into()),
        id: get("ID").unwrap_or_default(),
        kernel: read_trim(r.proc.join("sys/kernel/osrelease")).unwrap_or_default(),
        arch: std::env::consts::ARCH.to_string(),
        hostname: read_trim(r.proc.join("sys/kernel/hostname")).unwrap_or_default(),
        uptime_secs: uptime as u64,
        load_avg,
        session_type: std::env::var("XDG_SESSION_TYPE").ok().filter(|s| !s.is_empty()),
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok().filter(|s| !s.is_empty()),
        init: read_trim(r.proc.join("1/comm")).unwrap_or_default(),
        boot_mode: if efi.exists() { "UEFI".into() } else { "BIOS (legado)".into() },
        secure_boot,
        virtualization,
    }
}

// ---------------------------------------------------------------------------------------------
// Placa-mãe e BIOS (DMI)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct BoardInfo {
    pub board_vendor: String,
    pub board_name: String,
    pub board_version: String,
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
    pub system_vendor: String,
    pub product_name: String,
    pub chassis: String,
    pub is_laptop: bool,
}

/// Nome (em português) do tipo de chassi do SMBIOS.
fn chassis_name(code: u32) -> &'static str {
    match code {
        3 => "Desktop",
        4 => "Desktop compacto",
        5 => "Pizza box",
        6 => "Mini torre",
        7 => "Torre",
        8 => "Portátil",
        9 => "Notebook",
        10 => "Notebook",
        11 => "Handheld",
        13 => "All-in-one",
        14 => "Subnotebook",
        15 => "Space-saving",
        16 => "Lunch box",
        17 => "Servidor",
        23 => "Servidor rack",
        30 => "Tablet",
        31 => "Conversível",
        32 => "Destacável",
        35 => "Mini PC",
        36 => "Stick PC",
        _ => "Desconhecido",
    }
}

pub fn collect_board(r: &Roots) -> BoardInfo {
    let d = r.sys.join("class/dmi/id");
    let g = |n: &str| read_trim(d.join(n)).unwrap_or_default();
    let code: u32 = read_num(d.join("chassis_type")).unwrap_or(0);
    BoardInfo {
        board_vendor: g("board_vendor"),
        board_name: g("board_name"),
        board_version: g("board_version"),
        bios_vendor: g("bios_vendor"),
        bios_version: g("bios_version"),
        bios_date: g("bios_date"),
        system_vendor: g("sys_vendor"),
        product_name: g("product_name"),
        chassis: chassis_name(code).to_string(),
        is_laptop: matches!(code, 8 | 9 | 10 | 14 | 30 | 31 | 32),
    }
}

// ---------------------------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------------------------

/// Converte listas do kernel ("0-3,8,10-11") em números.
pub fn parse_cpu_list(s: &str) -> Vec<u32> {
    let mut v = Vec::new();
    for part in s.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                v.extend(a..=b);
            }
        } else if let Ok(n) = part.parse() {
            v.push(n);
        }
    }
    v
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheInfo {
    pub level: u8,
    pub kind: String,
    pub size_kib: u64,
    pub instances: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HybridInfo {
    pub p_cores: u32,
    pub p_threads: u32,
    pub e_cores: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuInfo {
    pub model: String,
    pub vendor: String,
    pub family: Option<u32>,
    pub model_id: Option<u32>,
    pub stepping: Option<u32>,
    pub microcode: Option<String>,
    pub sockets: u32,
    pub cores: u32,
    pub threads: u32,
    pub hybrid: Option<HybridInfo>,
    pub min_mhz: Option<f64>,
    pub max_mhz: Option<f64>,
    pub governor: Option<String>,
    pub scaling_driver: Option<String>,
    pub epp: Option<String>,
    pub boost: Option<bool>,
    pub caches: Vec<CacheInfo>,
    pub features: Vec<String>,
    pub vulnerabilities: Vec<(String, String)>,
}

const FEATURES: &[(&str, &str)] = &[
    ("sse4_2", "SSE4.2"),
    ("avx", "AVX"),
    ("avx2", "AVX2"),
    ("fma", "FMA3"),
    ("avx512f", "AVX-512"),
    ("avx_vnni", "AVX-VNNI"),
    ("amx_tile", "AMX"),
    ("aes", "AES-NI"),
    ("sha_ni", "SHA"),
    ("vmx", "VT-x"),
    ("svm", "AMD-V"),
];

fn lscpu_model() -> Option<String> {
    let out = run_stdout("lscpu", &[], Duration::from_secs(3))?;
    parse_colon_lines(&out).into_iter().find(|(k, _)| k == "Model name").map(|(_, v)| v)
}

pub fn collect_cpu(r: &Roots) -> CpuInfo {
    let text = fs::read_to_string(r.proc.join("cpuinfo")).unwrap_or_default();
    let blocks: Vec<Vec<(String, String)>> = text.split("\n\n").map(parse_colon_lines).filter(|b| !b.is_empty()).collect();
    let get = |b: &[(String, String)], k: &str| b.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
    let empty: Vec<(String, String)> = Vec::new();
    let first = blocks.first().unwrap_or(&empty);
    let num = |k: &str| get(first, k).and_then(|v| v.parse::<u32>().ok());

    let mut cpu = CpuInfo {
        model: get(first, "model name").or_else(|| get(first, "Hardware")).or_else(|| get(first, "Processor")).unwrap_or_default(),
        vendor: get(first, "vendor_id").unwrap_or_default(),
        family: num("cpu family"),
        model_id: num("model"),
        stepping: num("stepping"),
        microcode: get(first, "microcode"),
        ..Default::default()
    };
    if cpu.model.is_empty() {
        cpu.model = lscpu_model().unwrap_or_else(|| "CPU desconhecida".into());
    }
    cpu.vendor = match cpu.vendor.as_str() {
        "GenuineIntel" => "Intel".into(),
        "AuthenticAMD" => "AMD".into(),
        other => other.to_string(),
    };

    // Características (conjuntos de instruções) presentes
    if let Some(flags) = get(first, "flags").or_else(|| get(first, "Features")) {
        let set: BTreeSet<&str> = flags.split_whitespace().collect();
        cpu.features = FEATURES.iter().filter(|(k, _)| set.contains(k)).map(|(_, n)| n.to_string()).collect();
    }

    // Topologia: sockets, núcleos físicos e threads
    let cpu_root = r.sys.join("devices/system/cpu");
    let logical_ids: Vec<String> =
        dir_names(&cpu_root).into_iter().filter(|n| n.strip_prefix("cpu").is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))).collect();
    let mut cores = BTreeSet::new();
    let mut pkgs = BTreeSet::new();
    let mut core_of: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for name in &logical_ids {
        let t = cpu_root.join(name).join("topology");
        let (core, pkg) = (read_num::<u32>(t.join("core_id")), read_num::<u32>(t.join("physical_package_id")));
        if let (Some(c), Some(p)) = (core, pkg) {
            cores.insert((p, c));
            pkgs.insert(p);
            if let Ok(id) = name[3..].parse::<u32>() {
                core_of.insert(id, (p, c));
            }
        }
    }
    let logical_from_info = blocks.iter().filter(|b| get(b, "processor").is_some()).count() as u32;
    cpu.threads = if !logical_ids.is_empty() { logical_ids.len() as u32 } else { logical_from_info.max(1) };
    cpu.cores = if !cores.is_empty() { cores.len() as u32 } else { get(first, "cpu cores").and_then(|v| v.parse().ok()).unwrap_or(cpu.threads) };
    cpu.sockets = if !pkgs.is_empty() { pkgs.len() as u32 } else { 1 };

    // CPUs híbridas (Intel P-cores + E-cores)
    let list = |p: &str| read_trim(r.sys.join(p)).map(|s| parse_cpu_list(&s)).unwrap_or_default();
    let (p_list, e_list) = (list("devices/cpu_core/cpus"), list("devices/cpu_atom/cpus"));
    if !p_list.is_empty() && !e_list.is_empty() {
        let p_cores: BTreeSet<(u32, u32)> = p_list.iter().filter_map(|c| core_of.get(c).copied()).collect();
        cpu.hybrid = Some(HybridInfo {
            p_cores: if p_cores.is_empty() { p_list.len() as u32 } else { p_cores.len() as u32 },
            p_threads: p_list.len() as u32,
            e_cores: e_list.len() as u32,
        });
    }

    // Frequências e política de energia
    let f0 = cpu_root.join("cpu0/cpufreq");
    cpu.min_mhz = read_num::<f64>(f0.join("cpuinfo_min_freq")).map(|k| k / 1000.0);
    cpu.max_mhz = read_num::<f64>(f0.join("cpuinfo_max_freq")).map(|k| k / 1000.0);
    cpu.governor = read_trim(f0.join("scaling_governor"));
    cpu.scaling_driver = read_trim(f0.join("scaling_driver"));
    cpu.epp = read_trim(f0.join("energy_performance_preference"));
    cpu.boost = read_trim(cpu_root.join("cpufreq/boost"))
        .map(|v| v == "1")
        .or_else(|| read_trim(cpu_root.join("intel_pstate/no_turbo")).map(|v| v == "0"));

    cpu.caches = collect_caches(&cpu_root, &logical_ids);

    cpu.vulnerabilities = dir_names(cpu_root.join("vulnerabilities"))
        .into_iter()
        .filter_map(|n| read_trim(cpu_root.join("vulnerabilities").join(&n)).map(|v| (n, v)))
        .collect();
    cpu
}

/// Caches (L1d, L1i, L2, L3) e quantas instâncias existem, contando listas de CPUs compartilhadas distintas.
fn collect_caches(cpu_root: &std::path::Path, logical_ids: &[String]) -> Vec<CacheInfo> {
    let mut map: BTreeMap<(u8, String), (u64, BTreeSet<String>)> = BTreeMap::new();
    for name in logical_ids {
        let cdir = cpu_root.join(name).join("cache");
        for idx in dir_names(&cdir).into_iter().filter(|n| n.starts_with("index")) {
            let d = cdir.join(idx);
            let (Some(level), Some(kind)) = (read_num::<u8>(d.join("level")), read_trim(d.join("type"))) else { continue };
            let size = read_trim(d.join("size")).map(|s| parse_cache_size_kib(&s)).unwrap_or(0);
            let shared = read_trim(d.join("shared_cpu_list")).unwrap_or_else(|| name.clone());
            let e = map.entry((level, kind)).or_insert((size, BTreeSet::new()));
            e.1.insert(shared);
        }
    }
    map.into_iter()
        .map(|((level, kind), (size, set))| CacheInfo {
            level,
            kind: match kind.as_str() {
                "Data" => "dados".into(),
                "Instruction" => "instruções".into(),
                "Unified" => "unificado".into(),
                other => other.to_lowercase(),
            },
            size_kib: size,
            instances: set.len() as u32,
        })
        .collect()
}

fn parse_cache_size_kib(s: &str) -> u64 {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('K') {
        n.parse().unwrap_or(0)
    } else if let Some(n) = s.strip_suffix('M') {
        n.parse::<u64>().unwrap_or(0) * 1024
    } else {
        s.parse::<u64>().unwrap_or(0) / 1024
    }
}

// ---------------------------------------------------------------------------------------------
// Memória
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct RamModule {
    pub locator: String,
    pub bank: String,
    pub size_mib: u64,
    pub form_factor: String,
    pub kind: String,
    pub speed_mts: Option<u32>,
    pub configured_mts: Option<u32>,
    pub manufacturer: String,
    pub part_number: String,
    pub voltage: Option<f64>,
    pub rank: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ZramInfo {
    pub name: String,
    pub disk_size: u64,
    pub algorithm: String,
    pub orig_bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub free: u64,
    pub buffers: u64,
    pub cached: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub zram: Vec<ZramInfo>,
    pub thp: Option<String>,
    pub modules: Vec<RamModule>,
    /// Motivo de não haver módulos listados (normalmente: falta de privilégio).
    pub modules_note: Option<String>,
}

impl MemInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
    /// Tipo de memória (DDR4, DDR5...) dos módulos listados.
    pub fn ddr_type(&self) -> Option<String> {
        self.modules.iter().find(|m| !m.kind.is_empty() && m.kind != "Unknown").map(|m| m.kind.clone())
    }
}

/// Lê /proc/meminfo. `with_modules` consulta o dmidecode (pode precisar de privilégios).
pub fn collect_mem(r: &Roots, with_modules: bool) -> MemInfo {
    let kv = parse_colon_lines(&fs::read_to_string(r.proc.join("meminfo")).unwrap_or_default());
    let kb = |k: &str| {
        kv.iter().find(|(kk, _)| kk == k).and_then(|(_, v)| v.split_whitespace().next()?.parse::<u64>().ok()).map(|v| v * 1024).unwrap_or(0)
    };
    let mut m = MemInfo {
        total: kb("MemTotal"),
        available: kb("MemAvailable"),
        free: kb("MemFree"),
        buffers: kb("Buffers"),
        cached: kb("Cached"),
        swap_total: kb("SwapTotal"),
        swap_free: kb("SwapFree"),
        ..Default::default()
    };
    if m.available == 0 {
        m.available = m.free + m.buffers + m.cached;
    }
    for name in dir_names(r.sys.join("block")).into_iter().filter(|n| n.starts_with("zram")) {
        let d = r.sys.join("block").join(&name);
        let algo = read_trim(d.join("comp_algorithm")).map(|s| s.split('[').nth(1).and_then(|x| x.split(']').next()).unwrap_or(&s).to_string()).unwrap_or_default();
        let mm: Vec<u64> = read_trim(d.join("mm_stat")).map(|s| s.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
        m.zram.push(ZramInfo {
            name,
            disk_size: read_num(d.join("disksize")).unwrap_or(0),
            algorithm: algo,
            orig_bytes: mm.first().copied().unwrap_or(0),
            compressed_bytes: mm.get(1).copied().unwrap_or(0),
        });
    }
    m.thp = read_trim(r.sys.join("kernel/mm/transparent_hugepage/enabled"))
        .map(|s| s.split('[').nth(1).and_then(|x| x.split(']').next()).unwrap_or(&s).to_string());
    if with_modules {
        match run_maybe_priv("dmidecode", &["-t", "memory"], Duration::from_secs(8)) {
            Some(o) if o.ok() => {
                m.modules = parse_dmidecode_memory(&o.stdout);
                if m.modules.is_empty() {
                    m.modules_note = Some("o dmidecode não listou módulos (comum em máquinas virtuais)".into());
                }
            }
            Some(o) if o.combined().to_lowercase().contains("permission") => {
                m.modules_note = Some("precisa de privilégios para ler o SMBIOS: rode com sudo".into());
            }
            Some(o) => m.modules_note = Some(format!("dmidecode falhou: {}", o.last_line())),
            None => m.modules_note = Some("instale o dmidecode (e rode com sudo) para ver os módulos de RAM".into()),
        }
    }
    m
}

fn parse_size_mib(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    match it.next()? {
        "GB" => Some(n * 1024),
        "MB" => Some(n),
        "TB" => Some(n * 1024 * 1024),
        "kB" | "KB" => Some(n / 1024),
        _ => None,
    }
}

/// Interpreta a saída de `dmidecode -t memory` (blocos "Memory Device"); slots vazios são ignorados.
pub fn parse_dmidecode_memory(text: &str) -> Vec<RamModule> {
    let mut out = Vec::new();
    for block in text.split("\n\n").filter(|b| b.contains("Memory Device")) {
        let kv: BTreeMap<String, String> = block
            .lines()
            .filter_map(|l| l.trim().split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let Some(size) = kv.get("Size").and_then(|s| parse_size_mib(s)) else { continue };
        let first_num = |k: &str| kv.get(k).and_then(|v| v.split_whitespace().next()?.parse::<u32>().ok());
        let clean = |k: &str| {
            let v = kv.get(k).cloned().unwrap_or_default();
            if matches!(v.as_str(), "Not Specified" | "Unknown" | "NO DIMM" | "None") { String::new() } else { v }
        };
        out.push(RamModule {
            locator: clean("Locator"),
            bank: clean("Bank Locator"),
            size_mib: size,
            form_factor: clean("Form Factor"),
            kind: clean("Type"),
            speed_mts: first_num("Speed"),
            configured_mts: first_num("Configured Memory Speed").or_else(|| first_num("Configured Clock Speed")),
            manufacturer: clean("Manufacturer"),
            part_number: clean("Part Number").trim().to_string(),
            voltage: kv.get("Configured Voltage").and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok()),
            rank: first_num("Rank"),
        });
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Monitores (DRM + EDID)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct EdidInfo {
    pub manufacturer: String,
    pub product_code: u16,
    pub name: Option<String>,
    pub serial: Option<String>,
    pub width_cm: u32,
    pub height_cm: u32,
    pub year: Option<u32>,
    pub preferred: Option<(u32, u32, f64)>,
}

impl EdidInfo {
    pub fn diagonal_inches(&self) -> Option<f64> {
        (self.width_cm > 0 && self.height_cm > 0).then(|| ((self.width_cm.pow(2) + self.height_cm.pow(2)) as f64).sqrt() / 2.54)
    }
}

/// Lê o bloco base de 128 bytes do EDID. `None` se o cabeçalho for inválido.
pub fn parse_edid(b: &[u8]) -> Option<EdidInfo> {
    if b.len() < 128 || b[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    let id = u16::from_be_bytes([b[8], b[9]]);
    let letter = |v: u16| char::from(b'@' + (v & 0x1F) as u8);
    let manufacturer: String = [letter(id >> 10), letter(id >> 5), letter(id)].iter().collect();
    let mut e = EdidInfo {
        manufacturer,
        product_code: u16::from_le_bytes([b[10], b[11]]),
        width_cm: b[21] as u32,
        height_cm: b[22] as u32,
        year: (b[17] > 0).then(|| 1990 + b[17] as u32),
        ..Default::default()
    };
    for (i, off) in [54usize, 72, 90, 108].iter().enumerate() {
        let d = &b[*off..*off + 18];
        let pclk = u16::from_le_bytes([d[0], d[1]]) as u64;
        if pclk != 0 {
            if i == 0 {
                let hact = d[2] as u64 | ((d[4] as u64 & 0xF0) << 4);
                let hblank = d[3] as u64 | ((d[4] as u64 & 0x0F) << 8);
                let vact = d[5] as u64 | ((d[7] as u64 & 0xF0) << 4);
                let vblank = d[6] as u64 | ((d[7] as u64 & 0x0F) << 8);
                let total = (hact + hblank) * (vact + vblank);
                if total > 0 {
                    e.preferred = Some((hact as u32, vact as u32, pclk as f64 * 10_000.0 / total as f64));
                }
            }
        } else {
            let text = || {
                let raw: Vec<u8> = d[5..18].iter().copied().take_while(|c| *c != 0x0A).collect();
                String::from_utf8_lossy(&raw).trim().to_string()
            };
            match d[3] {
                0xFC => e.name = Some(text()),
                0xFF => e.serial = Some(text()),
                _ => {}
            }
        }
    }
    Some(e)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DisplayInfo {
    pub connector: String,
    pub enabled: bool,
    pub modes: Vec<String>,
    pub edid: Option<EdidInfo>,
}

/// Monitores conectados segundo o DRM (/sys/class/drm).
pub fn collect_displays(r: &Roots) -> Vec<DisplayInfo> {
    let base = r.sys.join("class/drm");
    let mut v = Vec::new();
    for name in dir_names(&base) {
        let Some(rest) = name.strip_prefix("card") else { continue };
        let Some((_, connector)) = rest.split_once('-') else { continue };
        let d = base.join(&name);
        if read_trim(d.join("status")).as_deref() != Some("connected") {
            continue;
        }
        v.push(DisplayInfo {
            connector: connector.to_string(),
            enabled: read_trim(d.join("enabled")).as_deref() == Some("enabled"),
            modes: read_lines(d.join("modes")).into_iter().take(6).collect(),
            edid: fs::read(d.join("edid")).ok().and_then(|b| parse_edid(&b)),
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), [0, 1, 2, 3, 8, 10, 11]);
        assert!(parse_cpu_list("").is_empty());
        assert_eq!(parse_cpu_list("5"), [5]);
    }

    #[test]
    fn collects_os_info() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "etc/os-release", "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\nID=ubuntu\n");
        write(d.path(), "proc/sys/kernel/osrelease", "6.8.0-60-generic\n");
        write(d.path(), "proc/sys/kernel/hostname", "pc-do-kaique\n");
        write(d.path(), "proc/uptime", "93784.12 400000.00\n");
        write(d.path(), "proc/loadavg", "0.52 0.40 0.31 1/500 12345\n");
        write(d.path(), "proc/1/comm", "systemd\n");
        fs::create_dir_all(d.path().join("sys/firmware/efi")).unwrap();
        let os = collect_os(&Roots::under(d.path()));
        assert_eq!(os.pretty_name, "Ubuntu 24.04.3 LTS");
        assert_eq!(os.kernel, "6.8.0-60-generic");
        assert_eq!(os.hostname, "pc-do-kaique");
        assert_eq!(os.uptime_secs, 93784);
        assert_eq!(os.load_avg, Some((0.52, 0.40, 0.31)));
        assert_eq!(os.boot_mode, "UEFI");
        assert_eq!(os.init, "systemd");
    }

    #[test]
    fn collects_board_info() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "sys/class/dmi/id/board_vendor", "ASUSTeK COMPUTER INC.\n");
        write(d.path(), "sys/class/dmi/id/board_name", "ROG STRIX B650-A\n");
        write(d.path(), "sys/class/dmi/id/chassis_type", "3\n");
        let b = collect_board(&Roots::under(d.path()));
        assert_eq!(b.board_name, "ROG STRIX B650-A");
        assert_eq!(b.chassis, "Desktop");
        assert!(!b.is_laptop);
        write(d.path(), "sys/class/dmi/id/chassis_type", "10\n");
        assert!(collect_board(&Roots::under(d.path())).is_laptop);
    }

    fn fake_cpu_tree(root: &Path) {
        let mut cpuinfo = String::new();
        for i in 0..4 {
            cpuinfo += &format!(
                "processor\t: {i}\nvendor_id\t: AuthenticAMD\ncpu family\t: 25\nmodel\t\t: 97\nmodel name\t: AMD Ryzen 7 7700X 8-Core Processor\nstepping\t: 2\nmicrocode\t: 0xa601206\ncpu MHz\t\t: 4500.000\nflags\t\t: fpu sse4_2 avx avx2 fma avx512f aes sha_ni svm\n\n"
            );
        }
        write(root, "proc/cpuinfo", &cpuinfo);
        for i in 0..4 {
            let (core, pkg) = (i / 2, 0);
            write(root, &format!("sys/devices/system/cpu/cpu{i}/topology/core_id"), &format!("{core}\n"));
            write(root, &format!("sys/devices/system/cpu/cpu{i}/topology/physical_package_id"), &format!("{pkg}\n"));
            // L1d/L1i por núcleo (compartilhado entre 2 threads), L3 compartilhado por todos
            let l1 = if i < 2 { "0-1" } else { "2-3" };
            for (idx, level, kind, size, shared) in [(0, 1, "Data", "32K", l1), (1, 1, "Instruction", "32K", l1), (2, 2, "Unified", "1024K", l1), (3, 3, "Unified", "32768K", "0-3")] {
                let b = format!("sys/devices/system/cpu/cpu{i}/cache/index{idx}");
                write(root, &format!("{b}/level"), &format!("{level}\n"));
                write(root, &format!("{b}/type"), &format!("{kind}\n"));
                write(root, &format!("{b}/size"), &format!("{size}\n"));
                write(root, &format!("{b}/shared_cpu_list"), &format!("{shared}\n"));
            }
        }
        write(root, "sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq", "5400000\n");
        write(root, "sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq", "400000\n");
        write(root, "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor", "powersave\n");
        write(root, "sys/devices/system/cpu/cpu0/cpufreq/scaling_driver", "amd-pstate-epp\n");
        write(root, "sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference", "balance_performance\n");
        write(root, "sys/devices/system/cpu/cpufreq/boost", "1\n");
        write(root, "sys/devices/system/cpu/vulnerabilities/meltdown", "Not affected\n");
        write(root, "sys/devices/system/cpu/vulnerabilities/spectre_v2", "Mitigation: Enhanced / Automatic IBRS\n");
    }

    #[test]
    fn collects_cpu_topology_caches_and_policy() {
        let d = tempfile::tempdir().unwrap();
        fake_cpu_tree(d.path());
        let c = collect_cpu(&Roots::under(d.path()));
        assert_eq!(c.model, "AMD Ryzen 7 7700X 8-Core Processor");
        assert_eq!(c.vendor, "AMD");
        assert_eq!((c.sockets, c.cores, c.threads), (1, 2, 4));
        assert_eq!(c.max_mhz, Some(5400.0));
        assert_eq!(c.min_mhz, Some(400.0));
        assert_eq!(c.governor.as_deref(), Some("powersave"));
        assert_eq!(c.scaling_driver.as_deref(), Some("amd-pstate-epp"));
        assert_eq!(c.boost, Some(true));
        assert_eq!(c.microcode.as_deref(), Some("0xa601206"));
        assert!(c.features.contains(&"AVX2".to_string()) && c.features.contains(&"AMD-V".to_string()));
        assert!(!c.features.contains(&"AMX".to_string()));
        let l1d = c.caches.iter().find(|x| x.level == 1 && x.kind == "dados").unwrap();
        assert_eq!((l1d.size_kib, l1d.instances), (32, 2));
        let l3 = c.caches.iter().find(|x| x.level == 3).unwrap();
        assert_eq!((l3.size_kib, l3.instances), (32768, 1));
        assert_eq!(c.vulnerabilities.len(), 2);
        assert!(c.hybrid.is_none());
    }

    #[test]
    fn detects_hybrid_cpu() {
        let d = tempfile::tempdir().unwrap();
        fake_cpu_tree(d.path());
        write(d.path(), "sys/devices/cpu_core/cpus", "0-1\n");
        write(d.path(), "sys/devices/cpu_atom/cpus", "2-3\n");
        let h = collect_cpu(&Roots::under(d.path())).hybrid.unwrap();
        assert_eq!((h.p_cores, h.p_threads, h.e_cores), (1, 2, 2));
    }

    #[test]
    fn collects_memory_and_zram() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "proc/meminfo", "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8192000 kB\nBuffers: 100 kB\nCached: 2000000 kB\nSwapTotal: 4096000 kB\nSwapFree: 4000000 kB\n");
        write(d.path(), "sys/block/zram0/disksize", "4294967296\n");
        write(d.path(), "sys/block/zram0/comp_algorithm", "lzo lzo-rle lz4 [zstd]\n");
        write(d.path(), "sys/block/zram0/mm_stat", "1000000 400000 450000 0 0 0 0 0\n");
        write(d.path(), "sys/kernel/mm/transparent_hugepage/enabled", "always [madvise] never\n");
        let m = collect_mem(&Roots::under(d.path()), false);
        assert_eq!(m.total, 16384000 * 1024);
        assert_eq!(m.used(), (16384000 - 8192000) * 1024);
        assert_eq!(m.swap_used(), 96000 * 1024);
        assert_eq!(m.zram[0].algorithm, "zstd");
        assert_eq!(m.zram[0].compressed_bytes, 400000);
        assert_eq!(m.thp.as_deref(), Some("madvise"));
    }

    #[test]
    fn parses_dmidecode_output() {
        let text = "# dmidecode 3.5\n\nHandle 0x0030, DMI type 17, 40 bytes\nMemory Device\n\tTotal Width: 64 bits\n\tSize: 16 GB\n\tForm Factor: DIMM\n\tLocator: DIMM_A1\n\tBank Locator: P0 CHANNEL A\n\tType: DDR5\n\tSpeed: 4800 MT/s\n\tManufacturer: Kingston\n\tPart Number: KF560C40-16  \n\tRank: 1\n\tConfigured Memory Speed: 4800 MT/s\n\tConfigured Voltage: 1.1 V\n\nHandle 0x0031, DMI type 17, 40 bytes\nMemory Device\n\tSize: No Module Installed\n\tLocator: DIMM_A2\n\nHandle 0x0032, DMI type 17, 40 bytes\nMemory Device\n\tSize: 8192 MB\n\tForm Factor: SODIMM\n\tLocator: DIMM_B1\n\tType: DDR4\n\tSpeed: 3200 MHz\n\tManufacturer: Not Specified\n\tPart Number: Unknown\n";
        let m = parse_dmidecode_memory(text);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].size_mib, m[0].kind.as_str(), m[0].speed_mts), (16384, "DDR5", Some(4800)));
        assert_eq!((m[0].manufacturer.as_str(), m[0].part_number.as_str(), m[0].voltage), ("Kingston", "KF560C40-16", Some(1.1)));
        assert_eq!(m[0].configured_mts, Some(4800));
        assert_eq!((m[1].size_mib, m[1].speed_mts), (8192, Some(3200)));
        assert!(m[1].manufacturer.is_empty() && m[1].part_number.is_empty());
    }

    /// Monta um bloco EDID base com um timing preferido de 2560x1440 a ~60 Hz e o nome "TESTE 27".
    fn sample_edid() -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        let id: u16 = (4 << 10) | (5 << 5) | 12; // "DEL"
        b[8..10].copy_from_slice(&id.to_be_bytes());
        b[10..12].copy_from_slice(&0x4242u16.to_le_bytes());
        b[17] = 33; // 2023
        b[21] = 60;
        b[22] = 34;
        let (hact, hblank, vact, vblank) = (2560u32, 160u32, 1440u32, 41u32);
        let pclk: u16 = 24150; // 241,5 MHz
        b[54..56].copy_from_slice(&pclk.to_le_bytes());
        b[56] = (hact & 0xFF) as u8;
        b[57] = (hblank & 0xFF) as u8;
        b[58] = (((hact >> 8) & 0xF) << 4 | ((hblank >> 8) & 0xF)) as u8;
        b[59] = (vact & 0xFF) as u8;
        b[60] = (vblank & 0xFF) as u8;
        b[61] = (((vact >> 8) & 0xF) << 4 | ((vblank >> 8) & 0xF)) as u8;
        // descritor de nome em 72
        b[75] = 0xFC;
        let name = b"TESTE 27\n";
        b[77..77 + name.len()].copy_from_slice(name);
        b
    }

    #[test]
    fn parses_edid() {
        let e = parse_edid(&sample_edid()).unwrap();
        assert_eq!(e.manufacturer, "DEL");
        assert_eq!(e.product_code, 0x4242);
        assert_eq!(e.name.as_deref(), Some("TESTE 27"));
        assert_eq!(e.year, Some(2023));
        let (w, h, hz) = e.preferred.unwrap();
        assert_eq!((w, h), (2560, 1440));
        assert!((hz - 59.95).abs() < 0.1, "hz = {hz}");
        assert!((e.diagonal_inches().unwrap() - 27.1).abs() < 0.2);
        assert!(parse_edid(&[0u8; 128]).is_none());
        assert!(parse_edid(&[0u8; 10]).is_none());
    }

    #[test]
    fn collects_connected_displays_only() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("sys/class/drm");
        write(d.path(), "sys/class/drm/card0-DP-1/status", "connected\n");
        write(d.path(), "sys/class/drm/card0-DP-1/enabled", "enabled\n");
        write(d.path(), "sys/class/drm/card0-DP-1/modes", "2560x1440\n1920x1080\n");
        fs::write(base.join("card0-DP-1/edid"), sample_edid()).unwrap();
        write(d.path(), "sys/class/drm/card0-HDMI-A-1/status", "disconnected\n");
        fs::create_dir_all(base.join("card0")).unwrap();
        fs::create_dir_all(base.join("renderD128")).unwrap();
        let v = collect_displays(&Roots::under(d.path()));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].connector, "DP-1");
        assert!(v[0].enabled);
        assert_eq!(v[0].modes[0], "2560x1440");
        assert_eq!(v[0].edid.as_ref().unwrap().name.as_deref(), Some("TESTE 27"));
    }
}
