//! Placas de vídeo: identificação (PCI), sensores e limites.
//!
//! * NVIDIA: `nvidia-smi --query-gpu` (driver proprietário). Sem ele, só a identificação pelo PCI.
//! * AMD: arquivos do driver `amdgpu` em /sys (uso, VRAM, hwmon, clocks DPM).
//! * Intel: frequência pelo i915/xe.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::pci::{self, PciDevice};
use crate::util::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    #[default]
    Other,
}

impl GpuVendor {
    pub fn from_pci(id: u16) -> Self {
        match id {
            0x10de => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            _ => GpuVendor::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Intel => "Intel",
            GpuVendor::Other => "Outro",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuInfo {
    pub index: usize,
    pub vendor: GpuVendor,
    pub name: String,
    pub pci_slot: String,
    pub pci_id: String,
    pub driver: Option<String>,
    pub driver_version: Option<String>,
    pub vbios: Option<String>,
    pub uuid: Option<String>,
    pub arch: Option<String>,
    pub compute_cap: Option<String>,
    /// GPU integrada (compartilha a RAM do sistema).
    pub integrated: bool,
    pub boot_vga: bool,
    pub vram_total: Option<u64>,
    pub vram_used: Option<u64>,
    pub util_gpu: Option<f64>,
    pub util_mem: Option<f64>,
    pub temp: Option<f64>,
    pub hotspot: Option<f64>,
    pub mem_temp: Option<f64>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    pub power_default_w: Option<f64>,
    pub power_min_w: Option<f64>,
    pub power_max_w: Option<f64>,
    pub clock_core: Option<f64>,
    pub clock_mem: Option<f64>,
    pub clock_core_max: Option<f64>,
    pub clock_mem_max: Option<f64>,
    pub fan_pct: Option<f64>,
    pub fan_rpm: Option<f64>,
    pub voltage_v: Option<f64>,
    pub pstate: Option<String>,
    /// Motivos que estão limitando o clock agora.
    pub throttle: Vec<String>,
    /// Limitado por temperatura (software ou hardware).
    pub thermal_throttle: bool,
    pub pcie_now: Option<String>,
    pub pcie_max: Option<String>,
    /// Por que faltam sensores (driver ausente, falta de permissão...).
    pub note: Option<String>,
}

impl GpuInfo {
    pub fn vram_pct(&self) -> Option<f64> {
        match (self.vram_used, self.vram_total) {
            (Some(u), Some(t)) if t > 0 => Some(u as f64 / t as f64 * 100.0),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// NVIDIA (nvidia-smi)
// ---------------------------------------------------------------------------------------------

/// Campos pedidos ao nvidia-smi, na ordem em que aparecem na saída.
const NV_FIELDS: &[&str] = &[
    "index",
    "uuid",
    "name",
    "pci.bus_id",
    "driver_version",
    "vbios_version",
    "memory.total",
    "memory.used",
    "utilization.gpu",
    "utilization.memory",
    "temperature.gpu",
    "power.draw",
    "power.limit",
    "power.default_limit",
    "power.min_limit",
    "power.max_limit",
    "clocks.current.graphics",
    "clocks.current.memory",
    "clocks.max.graphics",
    "clocks.max.memory",
    "fan.speed",
    "pstate",
    "pcie.link.gen.gpucurrent",
    "pcie.link.gen.max",
    "pcie.link.width.current",
    "pcie.link.width.max",
    "compute_cap",
    "clocks_event_reasons.active",
    "temperature.memory",
];

/// Nome antigo do campo, usado por drivers que ainda não conhecem o novo.
const NV_ALTERNATIVES: &[(&str, &str)] = &[("clocks_event_reasons.active", "clocks_throttle_reasons.active")];

fn nv_value<'a>(s: Option<&&'a str>) -> Option<&'a str> {
    let v = (*s?).trim();
    if v.is_empty() || v.starts_with('[') || v.eq_ignore_ascii_case("n/a") || v.eq_ignore_ascii_case("not supported") || v.eq_ignore_ascii_case("unknown error") {
        None
    } else {
        Some(v)
    }
}

/// Arquitetura a partir da capacidade de computação CUDA.
pub fn nvidia_arch(cc: &str) -> Option<&'static str> {
    Some(match cc.trim() {
        "2.0" | "2.1" => "Fermi",
        "3.0" | "3.2" | "3.5" | "3.7" => "Kepler",
        "5.0" | "5.2" | "5.3" => "Maxwell",
        "6.0" | "6.1" | "6.2" => "Pascal",
        "7.0" | "7.2" => "Volta",
        "7.5" => "Turing",
        "8.0" | "8.6" | "8.7" => "Ampere",
        "8.9" => "Ada Lovelace",
        "9.0" => "Hopper",
        "10.0" | "10.1" | "10.3" | "12.0" | "12.1" => "Blackwell",
        _ => return None,
    })
}

/// Traduz o bitmask de limitadores do NVML. Devolve (motivos, limitado por temperatura).
pub fn decode_throttle(mask: u64) -> (Vec<String>, bool) {
    let table: &[(u64, &str)] = &[
        (0x0000_0002, "clocks fixados por aplicação"),
        (0x0000_0004, "limite de energia (software)"),
        (0x0000_0008, "redução forçada por hardware"),
        (0x0000_0010, "sync boost"),
        (0x0000_0020, "limite térmico (software)"),
        (0x0000_0040, "limite térmico (hardware)"),
        (0x0000_0080, "freio de energia (hardware)"),
    ];
    let reasons = table.iter().filter(|(bit, _)| mask & bit != 0).map(|(_, t)| t.to_string()).collect();
    (reasons, mask & 0x60 != 0)
}

fn parse_hex_or_dec(s: &str) -> Option<u64> {
    let s = s.trim();
    s.strip_prefix("0x").map_or_else(|| s.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
}

/// Interpreta a saída CSV do nvidia-smi (`--format=csv,noheader,nounits`) para os `fields` pedidos.
pub fn parse_nvidia_csv(text: &str, fields: &[&str]) -> Vec<GpuInfo> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let cells: Vec<&str> = line.split(',').map(str::trim).collect();
        let get = |name: &str| -> Option<&str> { fields.iter().position(|f| *f == name).and_then(|i| nv_value(cells.get(i))) };
        let num = |name: &str| get(name).and_then(|v| v.parse::<f64>().ok());
        let mut g = GpuInfo { vendor: GpuVendor::Nvidia, driver: Some("nvidia".into()), ..Default::default() };
        g.index = get("index").and_then(|v| v.parse().ok()).unwrap_or(out.len());
        g.uuid = get("uuid").map(str::to_string);
        g.name = get("name").unwrap_or("NVIDIA GPU").to_string();
        // "00000000:01:00.0" -> "0000:01:00.0"
        g.pci_slot = get("pci.bus_id").map(|s| s.to_lowercase()).map(|s| s[s.len().saturating_sub(12)..].to_string()).unwrap_or_default();
        g.driver_version = get("driver_version").map(str::to_string);
        g.vbios = get("vbios_version").map(str::to_string);
        g.vram_total = num("memory.total").map(|m| (m * 1024.0 * 1024.0) as u64);
        g.vram_used = num("memory.used").map(|m| (m * 1024.0 * 1024.0) as u64);
        g.util_gpu = num("utilization.gpu");
        g.util_mem = num("utilization.memory");
        g.temp = num("temperature.gpu");
        g.mem_temp = num("temperature.memory");
        g.power_w = num("power.draw");
        g.power_limit_w = num("power.limit");
        g.power_default_w = num("power.default_limit");
        g.power_min_w = num("power.min_limit");
        g.power_max_w = num("power.max_limit");
        g.clock_core = num("clocks.current.graphics");
        g.clock_mem = num("clocks.current.memory");
        g.clock_core_max = num("clocks.max.graphics");
        g.clock_mem_max = num("clocks.max.memory");
        g.fan_pct = num("fan.speed");
        g.pstate = get("pstate").map(str::to_string);
        let gen_now = get("pcie.link.gen.gpucurrent").or_else(|| get("pcie.link.gen.current"));
        g.pcie_now = gen_now.zip(get("pcie.link.width.current")).map(|(a, b)| format!("Gen{a} x{b}"));
        g.pcie_max = get("pcie.link.gen.max").zip(get("pcie.link.width.max")).map(|(a, b)| format!("Gen{a} x{b}"));
        g.compute_cap = get("compute_cap").map(str::to_string);
        g.arch = g.compute_cap.as_deref().and_then(nvidia_arch).map(str::to_string);
        if let Some(mask) = get("clocks_event_reasons.active").or_else(|| get("clocks_throttle_reasons.active")).and_then(parse_hex_or_dec) {
            let (reasons, thermal) = decode_throttle(mask);
            g.throttle = reasons;
            g.thermal_throttle = thermal;
        }
        out.push(g);
    }
    out
}

/// Extrai o nome do campo inválido de "Field "xyz" is not a valid field to query."
fn invalid_field(stderr: &str) -> Option<String> {
    let start = stderr.find("Field \"")? + 7;
    let end = stderr[start..].find('"')? + start;
    Some(stderr[start..end].to_string())
}

/// Consulta o nvidia-smi. Remove sozinho os campos que o driver instalado não conhece.
pub fn query_nvidia() -> Result<Vec<GpuInfo>, String> {
    if !have("nvidia-smi") {
        return Err("nvidia-smi não encontrado".into());
    }
    let mut fields: Vec<&str> = NV_FIELDS.to_vec();
    for _ in 0..8 {
        let arg = format!("--query-gpu={}", fields.join(","));
        let out = run("nvidia-smi", &[&arg, "--format=csv,noheader,nounits"], Duration::from_secs(8)).ok_or("não consegui executar o nvidia-smi")?;
        if out.timed_out {
            return Err("o nvidia-smi não respondeu a tempo".into());
        }
        if out.ok() {
            return Ok(parse_nvidia_csv(&out.stdout, &fields));
        }
        let err = out.combined();
        let Some(bad) = invalid_field(&err) else { return Err(out.last_line()) };
        let Some(pos) = fields.iter().position(|f| *f == bad) else { return Err(out.last_line()) };
        fields.remove(pos);
        if let Some((_, alt)) = NV_ALTERNATIVES.iter().find(|(old, _)| *old == bad) {
            fields.push(alt);
        }
    }
    Err("o nvidia-smi recusou a consulta".into())
}

// ---------------------------------------------------------------------------------------------
// AMD (amdgpu)
// ---------------------------------------------------------------------------------------------

/// Interpreta `pp_dpm_sclk`/`pp_dpm_mclk`: devolve (clock atual em MHz, clock máximo em MHz).
pub fn parse_dpm(text: &str) -> (Option<f64>, Option<f64>) {
    let mut cur = None;
    let mut max: Option<f64> = None;
    for l in text.lines() {
        let Some((_, rest)) = l.split_once(':') else { continue };
        let mhz: Option<f64> = rest.trim().trim_end_matches('*').trim().to_lowercase().trim_end_matches("mhz").trim().parse().ok();
        if let Some(m) = mhz {
            max = Some(max.map_or(m, |x| x.max(m)));
            if rest.trim_end().ends_with('*') {
                cur = Some(m);
            }
        }
    }
    (cur, max)
}

fn first_hwmon(dev_dir: &Path) -> Option<PathBuf> {
    let base = dev_dir.join("hwmon");
    dir_names(&base).into_iter().find(|n| n.starts_with("hwmon")).map(|n| base.join(n))
}

fn fill_amd(g: &mut GpuInfo, dev: &Path) {
    g.util_gpu = read_num(dev.join("gpu_busy_percent"));
    g.util_mem = read_num(dev.join("mem_busy_percent"));
    g.vram_total = read_num(dev.join("mem_info_vram_total"));
    g.vram_used = read_num(dev.join("mem_info_vram_used"));
    g.vbios = read_trim(dev.join("vbios_version"));
    if let Some(t) = read_trim(dev.join("pp_dpm_sclk")) {
        let (c, m) = parse_dpm(&t);
        g.clock_core = c;
        g.clock_core_max = m;
    }
    if let Some(t) = read_trim(dev.join("pp_dpm_mclk")) {
        let (c, m) = parse_dpm(&t);
        g.clock_mem = c;
        g.clock_mem_max = m;
    }
    g.pstate = read_trim(dev.join("power_dpm_force_performance_level"));
    if let Some(h) = first_hwmon(dev) {
        let temp = |n: &str| read_num::<f64>(h.join(n)).map(|v| v / 1000.0);
        g.temp = temp("temp1_input");
        g.hotspot = temp("temp2_input");
        g.mem_temp = temp("temp3_input");
        let watts = |n: &str| read_num::<f64>(h.join(n)).map(|v| v / 1_000_000.0);
        g.power_w = watts("power1_average").or_else(|| watts("power1_input"));
        g.power_limit_w = watts("power1_cap");
        g.power_max_w = watts("power1_cap_max");
        g.power_min_w = watts("power1_cap_min");
        g.power_default_w = watts("power1_cap_default");
        g.fan_rpm = read_num(h.join("fan1_input"));
        g.fan_pct = match (read_num::<f64>(h.join("pwm1")), read_num::<f64>(h.join("pwm1_max"))) {
            (Some(p), Some(m)) if m > 0.0 => Some(p / m * 100.0),
            (Some(p), None) => Some(p / 255.0 * 100.0),
            _ => None,
        };
        g.voltage_v = read_num::<f64>(h.join("in0_input")).map(|v| v / 1000.0);
        // freq1/freq2 do hwmon são os clocks reais quando o DPM não informa o atual
        let freq = |n: &str| read_num::<f64>(h.join(n)).map(|v| v / 1_000_000.0);
        g.clock_core = g.clock_core.or_else(|| freq("freq1_input"));
        g.clock_mem = g.clock_mem.or_else(|| freq("freq2_input"));
    }
    // APU: pouca VRAM dedicada e o resto vem da RAM (GTT)
    if let (Some(v), Some(gtt)) = (g.vram_total, read_num::<u64>(dev.join("mem_info_gtt_total"))) {
        g.integrated = v <= 2 * 1024 * 1024 * 1024 && gtt > v * 2;
    }
}

// ---------------------------------------------------------------------------------------------
// Intel (i915 / xe)
// ---------------------------------------------------------------------------------------------

fn fill_intel(g: &mut GpuInfo, dev: &Path) {
    let drm = dev.join("drm");
    let card = dir_names(&drm).into_iter().find(|n| n.starts_with("card") && !n.contains('-'));
    if let Some(card) = card {
        let c = drm.join(&card);
        g.clock_core = read_num(c.join("gt_cur_freq_mhz")).or_else(|| read_num(c.join("gt/gt0/rps_cur_freq_mhz")));
        g.clock_core_max = read_num(c.join("gt_max_freq_mhz")).or_else(|| read_num(c.join("gt/gt0/rps_max_freq_mhz")));
    }
    // Driver xe
    if g.clock_core.is_none() {
        let f = dev.join("tile0/gt0/freq0");
        g.clock_core = read_num(f.join("cur_freq"));
        g.clock_core_max = read_num(f.join("max_freq"));
    }
    if let Some(h) = first_hwmon(dev) {
        g.temp = read_num::<f64>(h.join("temp1_input")).map(|v| v / 1000.0);
        g.power_w = read_num::<f64>(h.join("power1_input")).map(|v| v / 1_000_000.0);
    }
    // GPU Intel no barramento 00 costuma ser integrada; placas Arc ficam em outro barramento
    g.integrated = g.pci_slot.contains(":00:02") || g.pci_slot.starts_with("0000:00:");
    if g.integrated {
        g.vram_total = None;
    }
}

// ---------------------------------------------------------------------------------------------
// Coleta
// ---------------------------------------------------------------------------------------------

fn base_from_pci(d: &PciDevice, index: usize) -> GpuInfo {
    let vendor = GpuVendor::from_pci(d.vendor);
    let name = if d.device_name.is_empty() { format!("{} {}", vendor.name(), d.id_string()) } else { pci::pretty_device_name(&d.vendor_name, &d.device_name) };
    GpuInfo {
        index,
        vendor,
        name,
        pci_slot: d.slot.clone(),
        pci_id: d.id_string(),
        driver: d.driver.clone(),
        boot_vga: d.boot_vga,
        pcie_now: d.link_now(),
        pcie_max: d.link_max(),
        ..Default::default()
    }
}

/// Todas as placas de vídeo do sistema, com os sensores que o driver instalado oferece.
pub fn collect(r: &Roots) -> Vec<GpuInfo> {
    let devices: Vec<PciDevice> = pci::list(r).into_iter().filter(|d| (d.class >> 16) == 0x03 && (d.class >> 8) & 0xff != 0x80).collect();
    let real = r.sys == Path::new("/sys");
    let mut nvidia = None;
    let mut gpus: Vec<GpuInfo> = Vec::new();
    for (i, d) in devices.iter().enumerate() {
        let mut g = base_from_pci(d, i);
        let dev_dir = r.sys.join("bus/pci/devices").join(&d.slot);
        match g.vendor {
            GpuVendor::Nvidia => {
                if real && nvidia.is_none() {
                    nvidia = Some(query_nvidia());
                }
                match &nvidia {
                    Some(Ok(list)) => {
                        if let Some(n) = list.iter().find(|n| n.pci_slot == d.slot).or_else(|| (devices.iter().filter(|x| x.vendor == 0x10de).count() == 1).then(|| list.first()).flatten()) {
                            let (index, boot, pci_id) = (g.index, g.boot_vga, g.pci_id.clone());
                            g = n.clone();
                            g.index = index;
                            g.boot_vga = boot;
                            g.pci_id = pci_id;
                            g.pci_slot = d.slot.clone();
                            g.driver = d.driver.clone().or(g.driver);
                        }
                    }
                    Some(Err(e)) => g.note = Some(format!("sem sensores NVIDIA: {e}. Instale o driver proprietário para ver uso, temperatura e consumo.")),
                    None => g.note = Some("sensores NVIDIA indisponíveis neste ambiente".into()),
                }
                if g.name.starts_with("NVIDIA GPU") || g.name.is_empty() {
                    g.name = base_from_pci(d, i).name;
                }
            }
            GpuVendor::Amd => {
                fill_amd(&mut g, &dev_dir);
                if g.driver.as_deref() != Some("amdgpu") && g.driver.as_deref() != Some("radeon") {
                    g.note = Some("o driver amdgpu não está carregado".into());
                }
            }
            GpuVendor::Intel => fill_intel(&mut g, &dev_dir),
            GpuVendor::Other => {}
        }
        gpus.push(g);
    }
    gpus
}

/// Placa principal: a dedicada com mais VRAM, ou a de boot.
pub fn primary(gpus: &[GpuInfo]) -> Option<&GpuInfo> {
    gpus.iter()
        .filter(|g| !g.integrated)
        .max_by_key(|g| g.vram_total.unwrap_or(0))
        .or_else(|| gpus.iter().find(|g| g.boot_vga))
        .or_else(|| gpus.first())
}

/// Texto curto do limitador de clock: "nenhum" ou os motivos.
pub fn throttle_text(g: &GpuInfo) -> String {
    if g.throttle.is_empty() { "nenhum".into() } else { g.throttle.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "0, GPU-1234-abcd, NVIDIA GeForce RTX 4070, 00000000:01:00.0, 560.35.03, 95.04.3F.00.88, 12282, 1534, 3, 1, 41, 15.23, 200.00, 200.00, 100.00, 220.00, 210, 405, 3105, 10501, 0, P8, 4, 4, 1, 16, 8.9, 0x0000000000000004, [N/A]\n";

    #[test]
    fn parses_nvidia_smi_output() {
        let mut fields: Vec<&str> = NV_FIELDS.to_vec();
        // este exemplo usa a ordem exata de NV_FIELDS
        assert_eq!(fields.len(), SAMPLE.split(',').count());
        let v = parse_nvidia_csv(SAMPLE, &fields);
        assert_eq!(v.len(), 1);
        let g = &v[0];
        assert_eq!(g.name, "NVIDIA GeForce RTX 4070");
        assert_eq!(g.pci_slot, "0000:01:00.0");
        assert_eq!(g.driver_version.as_deref(), Some("560.35.03"));
        assert_eq!(g.vram_total, Some(12282 * 1024 * 1024));
        assert_eq!(g.util_gpu, Some(3.0));
        assert_eq!(g.temp, Some(41.0));
        assert_eq!(g.power_w, Some(15.23));
        assert_eq!(g.power_limit_w, Some(200.0));
        assert_eq!(g.power_max_w, Some(220.0));
        assert_eq!(g.clock_core, Some(210.0));
        assert_eq!(g.clock_mem_max, Some(10501.0));
        assert_eq!(g.fan_pct, Some(0.0));
        assert_eq!(g.pstate.as_deref(), Some("P8"));
        assert_eq!(g.pcie_now.as_deref(), Some("Gen4 x1"));
        assert_eq!(g.pcie_max.as_deref(), Some("Gen4 x16"));
        assert_eq!(g.arch.as_deref(), Some("Ada Lovelace"));
        assert_eq!(g.mem_temp, None, "[N/A] vira None");
        assert_eq!(g.throttle, ["limite de energia (software)"]);
        assert!(!g.thermal_throttle);
        assert!((g.vram_pct().unwrap() - 12.5).abs() < 0.1);
        // campo com nome antigo
        let i = fields.iter().position(|f| *f == "clocks_event_reasons.active").unwrap();
        fields[i] = "clocks_throttle_reasons.active";
        let g2 = &parse_nvidia_csv(SAMPLE, &fields)[0];
        assert_eq!(g2.throttle.len(), 1);
    }

    #[test]
    fn decodes_throttle_masks() {
        assert_eq!(decode_throttle(0x1), (vec![], false), "GPU ociosa não é limitação");
        let (r, thermal) = decode_throttle(0x60);
        assert_eq!(r.len(), 2);
        assert!(thermal);
        let (r, thermal) = decode_throttle(0x4 | 0x80);
        assert_eq!(r.len(), 2);
        assert!(!thermal);
        assert_eq!(parse_hex_or_dec("0x0000000000000020"), Some(0x20));
        assert_eq!(parse_hex_or_dec("32"), Some(32));
    }

    #[test]
    fn maps_compute_capability_to_architecture() {
        assert_eq!(nvidia_arch("12.0"), Some("Blackwell"));
        assert_eq!(nvidia_arch("8.6"), Some("Ampere"));
        assert_eq!(nvidia_arch("7.5"), Some("Turing"));
        assert_eq!(nvidia_arch("6.1"), Some("Pascal"));
        assert_eq!(nvidia_arch("99.9"), None);
    }

    #[test]
    fn extracts_invalid_field_name() {
        assert_eq!(invalid_field("Field \"clocks_event_reasons.active\" is not a valid field to query.\n"), Some("clocks_event_reasons.active".into()));
        assert_eq!(invalid_field("NVIDIA-SMI has failed"), None);
    }

    #[test]
    fn parses_dpm_tables() {
        let t = "0: 500Mhz\n1: 1250Mhz\n2: 2650Mhz *\n";
        assert_eq!(parse_dpm(t), (Some(2650.0), Some(2650.0)));
        let t = "0: 96Mhz\n1: 1000Mhz *\n2: 1500Mhz\n";
        assert_eq!(parse_dpm(t), (Some(1000.0), Some(1500.0)));
        assert_eq!(parse_dpm(""), (None, None));
    }

    fn write(root: &Path, rel: &str, content: &str) {
        use std::fs;
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn collects_amd_gpu_from_sysfs() {
        let d = tempfile::tempdir().unwrap();
        let dev = "sys/bus/pci/devices/0000:03:00.0";
        for (f, v) in [("vendor", "0x1002"), ("device", "0x744c"), ("class", "0x030000"), ("boot_vga", "1"), ("current_link_speed", "16.0 GT/s PCIe"), ("current_link_width", "16"), ("max_link_speed", "16.0 GT/s PCIe"), ("max_link_width", "16")] {
            write(d.path(), &format!("{dev}/{f}"), v);
        }
        write(d.path(), &format!("{dev}/gpu_busy_percent"), "87\n");
        write(d.path(), &format!("{dev}/mem_info_vram_total"), "25753026560\n");
        write(d.path(), &format!("{dev}/mem_info_vram_used"), "8589934592\n");
        write(d.path(), &format!("{dev}/pp_dpm_sclk"), "0: 500Mhz\n1: 2000Mhz *\n2: 2500Mhz\n");
        write(d.path(), &format!("{dev}/pp_dpm_mclk"), "0: 96Mhz\n1: 1249Mhz *\n");
        write(d.path(), &format!("{dev}/power_dpm_force_performance_level"), "auto\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/temp1_input"), "65000\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/temp2_input"), "78000\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/power1_average"), "250000000\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/power1_cap"), "300000000\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/power1_cap_max"), "327000000\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/fan1_input"), "1800\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/pwm1"), "128\n");
        write(d.path(), &format!("{dev}/hwmon/hwmon3/pwm1_max"), "255\n");
        let g = collect(&Roots::under(d.path()));
        assert_eq!(g.len(), 1);
        let g = &g[0];
        assert_eq!(g.vendor, GpuVendor::Amd);
        assert_eq!(g.util_gpu, Some(87.0));
        assert_eq!(g.temp, Some(65.0));
        assert_eq!(g.hotspot, Some(78.0));
        assert_eq!(g.power_w, Some(250.0));
        assert_eq!(g.power_limit_w, Some(300.0));
        assert_eq!(g.clock_core, Some(2000.0));
        assert_eq!(g.clock_core_max, Some(2500.0));
        assert_eq!(g.clock_mem, Some(1249.0));
        assert_eq!(g.fan_rpm, Some(1800.0));
        assert!((g.fan_pct.unwrap() - 50.2).abs() < 0.1);
        assert_eq!(g.pcie_now.as_deref(), Some("Gen4 x16"));
        assert!(g.boot_vga);
        assert!(!g.integrated);
        assert_eq!(primary(std::slice::from_ref(g)).unwrap().index, 0);
    }

    #[test]
    fn collects_intel_igpu_frequency() {
        let d = tempfile::tempdir().unwrap();
        let dev = "sys/bus/pci/devices/0000:00:02.0";
        for (f, v) in [("vendor", "0x8086"), ("device", "0x4680"), ("class", "0x030000"), ("boot_vga", "1")] {
            write(d.path(), &format!("{dev}/{f}"), v);
        }
        write(d.path(), &format!("{dev}/drm/card0/gt_cur_freq_mhz"), "350\n");
        write(d.path(), &format!("{dev}/drm/card0/gt_max_freq_mhz"), "1450\n");
        let g = &collect(&Roots::under(d.path()))[0];
        assert_eq!(g.vendor, GpuVendor::Intel);
        assert_eq!(g.clock_core, Some(350.0));
        assert_eq!(g.clock_core_max, Some(1450.0));
        assert!(g.integrated);
    }

    #[test]
    fn primary_prefers_discrete_gpu_with_most_vram() {
        let igpu = GpuInfo { index: 0, integrated: true, boot_vga: true, ..Default::default() };
        let small = GpuInfo { index: 1, vram_total: Some(4 << 30), ..Default::default() };
        let big = GpuInfo { index: 2, vram_total: Some(12 << 30), ..Default::default() };
        let all = [igpu.clone(), small, big];
        assert_eq!(primary(&all).unwrap().index, 2);
        assert_eq!(primary(&[igpu]).unwrap().index, 0);
        assert!(primary(&[]).is_none());
    }

    #[test]
    fn ignores_non_gpu_display_class_entries() {
        let d = tempfile::tempdir().unwrap();
        let dev = "sys/bus/pci/devices/0000:00:1f.3";
        for (f, v) in [("vendor", "0x8086"), ("device", "0x7a50"), ("class", "0x040300")] {
            write(d.path(), &format!("{dev}/{f}"), v);
        }
        assert!(collect(&Roots::under(d.path())).is_empty());
    }
}
