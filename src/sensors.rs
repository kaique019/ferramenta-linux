//! Sensores em tempo real: temperaturas, ventoinhas, tensões e consumo (hwmon e zonas térmicas),
//! carga e clock por núcleo (/proc/stat e cpufreq) e consumo da CPU (RAPL).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

use crate::util::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SensorKind {
    Temp,
    Fan,
    Voltage,
    Power,
    Current,
    Freq,
    Pwm,
}

impl SensorKind {
    pub fn unit(self) -> &'static str {
        match self {
            SensorKind::Temp => "°C",
            SensorKind::Fan => "RPM",
            SensorKind::Voltage => "V",
            SensorKind::Power => "W",
            SensorKind::Current => "A",
            SensorKind::Freq => "MHz",
            SensorKind::Pwm => "%",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            SensorKind::Temp => "Temperatura",
            SensorKind::Fan => "Ventoinha",
            SensorKind::Voltage => "Tensão",
            SensorKind::Power => "Consumo",
            SensorKind::Current => "Corrente",
            SensorKind::Freq => "Clock",
            SensorKind::Pwm => "Controle da ventoinha",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SensorReading {
    /// Nome do chip como o kernel informa ("coretemp", "k10temp", "nvme"...).
    pub chip: String,
    /// Nome para mostrar ("CPU Intel (coretemp)", "NVMe Samsung SSD 990").
    pub chip_display: String,
    pub kind: SensorKind,
    pub label: String,
    pub value: f64,
    pub max: Option<f64>,
    pub crit: Option<f64>,
}

impl SensorReading {
    pub fn unit(&self) -> &'static str {
        self.kind.unit()
    }

    pub fn text(&self) -> String {
        match self.kind {
            SensorKind::Temp => format!("{:.0} °C", self.value),
            SensorKind::Fan => format!("{:.0} RPM", self.value),
            SensorKind::Voltage => format!("{:.3} V", self.value),
            SensorKind::Power => format!("{:.1} W", self.value),
            SensorKind::Current => format!("{:.2} A", self.value),
            SensorKind::Freq => fmt_mhz(self.value),
            SensorKind::Pwm => format!("{:.0} %", self.value),
        }
    }
}

/// Nome amigável de um chip do hwmon.
pub fn chip_display(chip: &str, model: Option<&str>) -> String {
    let base = chip.trim_end_matches(|c: char| c == '_' || c.is_ascii_digit());
    match base {
        "coretemp" => "CPU Intel (coretemp)".into(),
        "k10temp" => "CPU AMD (k10temp)".into(),
        "zenpower" => "CPU AMD (zenpower)".into(),
        "amdgpu" => "GPU AMD (amdgpu)".into(),
        "nouveau" => "GPU NVIDIA (nouveau)".into(),
        "i915" | "xe" => "GPU Intel".into(),
        "nvme" => format!("NVMe{}", model.map(|m| format!(" {m}")).unwrap_or_default()),
        "drivetemp" => format!("Disco{}", model.map(|m| format!(" {m}")).unwrap_or_default()),
        "acpitz" => "Zona térmica ACPI".into(),
        "iwlwifi" | "ath10k_hwmon" | "ath11k_hwmon" | "mt7915_phy" => format!("Wi-Fi ({base})"),
        "BAT" | "BAT0" | "BAT1" => "Bateria".into(),
        "ucsi_source_psy_USBC000:001" => "USB-C".into(),
        "cpu_thermal" | "soc_thermal" => "SoC (térmico)".into(),
        _ => chip.to_string(),
    }
}

/// Divide "temp3_input" em ("temp", 3, "input"). `None` se não seguir o padrão do hwmon.
fn split_attr(name: &str) -> Option<(&str, u32, &str)> {
    let digits_start = name.find(|c: char| c.is_ascii_digit())?;
    let (prefix, rest) = name.split_at(digits_start);
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let (num, suffix) = rest.split_at(digits_end);
    let suffix = suffix.strip_prefix('_').unwrap_or(suffix);
    Some((prefix, num.parse().ok()?, suffix))
}

fn kind_of(prefix: &str) -> Option<SensorKind> {
    Some(match prefix {
        "temp" => SensorKind::Temp,
        "fan" => SensorKind::Fan,
        "in" => SensorKind::Voltage,
        "power" => SensorKind::Power,
        "curr" => SensorKind::Current,
        "freq" => SensorKind::Freq,
        "pwm" => SensorKind::Pwm,
        _ => return None,
    })
}

/// Converte o valor bruto do hwmon na unidade final (°C, RPM, V, W, A, MHz, %).
fn scale(kind: SensorKind, raw: f64) -> f64 {
    match kind {
        SensorKind::Temp => raw / 1000.0,
        SensorKind::Fan => raw,
        SensorKind::Voltage => raw / 1000.0,
        SensorKind::Power => raw / 1_000_000.0,
        SensorKind::Current => raw / 1000.0,
        SensorKind::Freq => raw / 1_000_000.0,
        SensorKind::Pwm => raw / 255.0 * 100.0,
    }
}

/// Lê todos os chips do hwmon e as zonas térmicas que não aparecem no hwmon.
pub fn read_all(r: &Roots) -> Vec<SensorReading> {
    let mut out = Vec::new();
    let base = r.sys.join("class/hwmon");
    let mut chips_seen: Vec<String> = Vec::new();
    for h in dir_names(&base) {
        let dir = base.join(&h);
        let chip = read_trim(dir.join("name")).unwrap_or_else(|| h.clone());
        let model = read_trim(dir.join("device/model"));
        let display = chip_display(&chip, model.as_deref());
        chips_seen.push(chip.clone());
        read_hwmon_dir(&dir, &chip, &display, &mut out);
        // Drivers antigos colocam os atributos em device/
        let dev = dir.join("device");
        if dev.is_dir() && !dir_names(&dev).iter().any(|n| n.starts_with("hwmon")) && out.iter().all(|s| s.chip != chip) {
            read_hwmon_dir(&dev, &chip, &display, &mut out);
        }
    }
    // Zonas térmicas
    let tz = r.sys.join("class/thermal");
    for z in dir_names(&tz).into_iter().filter(|n| n.starts_with("thermal_zone")) {
        let d = tz.join(&z);
        let (Some(kind), Some(t)) = (read_trim(d.join("type")), read_num::<f64>(d.join("temp"))) else { continue };
        if chips_seen.iter().any(|c| c == &kind) {
            continue;
        }
        out.push(SensorReading {
            chip: format!("thermal:{kind}"),
            chip_display: format!("Zona térmica ({kind})"),
            kind: SensorKind::Temp,
            label: z.clone(),
            value: t / 1000.0,
            max: None,
            crit: None,
        });
    }
    out
}

fn read_hwmon_dir(dir: &Path, chip: &str, display: &str, out: &mut Vec<SensorReading>) {
    let names = dir_names(dir);
    for name in &names {
        let Some((prefix, idx, suffix)) = split_attr(name) else { continue };
        let Some(kind) = kind_of(prefix) else { continue };
        let is_value = match kind {
            SensorKind::Power => suffix == "average" || suffix == "input",
            SensorKind::Pwm => suffix.is_empty(),
            _ => suffix == "input",
        };
        if !is_value {
            continue;
        }
        // Dois sensores de potência (average e input) para o mesmo índice: usa só um.
        if kind == SensorKind::Power && suffix == "input" && names.iter().any(|n| n == &format!("power{idx}_average")) {
            continue;
        }
        let Some(raw) = read_num::<f64>(dir.join(name)) else { continue };
        let label = read_trim(dir.join(format!("{prefix}{idx}_label"))).unwrap_or_else(|| format!("{prefix}{idx}"));
        let extra = |s: &str| read_num::<f64>(dir.join(format!("{prefix}{idx}_{s}"))).map(|v| scale(kind, v));
        out.push(SensorReading {
            chip: chip.to_string(),
            chip_display: display.to_string(),
            kind,
            label,
            value: scale(kind, raw),
            max: if kind == SensorKind::Temp || kind == SensorKind::Fan { extra("max") } else { None },
            crit: if kind == SensorKind::Temp { extra("crit") } else { None },
        });
    }
}

/// Temperatura do pacote da CPU (°C), escolhendo o sensor mais representativo disponível.
pub fn cpu_temperature(readings: &[SensorReading]) -> Option<f64> {
    fn temps<'a>(readings: &'a [SensorReading], chip: &'a str) -> impl Iterator<Item = &'a SensorReading> {
        readings.iter().filter(move |s| s.kind == SensorKind::Temp && s.chip == chip)
    }
    // AMD: Tdie > Tctl
    for chip in ["k10temp", "zenpower"] {
        for want in ["Tdie", "Tctl"] {
            if let Some(s) = temps(readings, chip).find(|s| s.label == want) {
                return Some(s.value);
            }
        }
        if let Some(s) = temps(readings, chip).next() {
            return Some(s.value);
        }
    }
    // Intel: "Package id 0"
    if let Some(s) = temps(readings, "coretemp").find(|s| s.label.starts_with("Package id")) {
        return Some(s.value);
    }
    if let Some(m) = temps(readings, "coretemp").map(|s| s.value).fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v)))) {
        return Some(m);
    }
    for chip in ["thermal:x86_pkg_temp", "thermal:cpu_thermal", "thermal:soc_thermal", "cpu_thermal", "soc_thermal", "thermal:TCPU", "thermal:acpitz", "acpitz"] {
        if let Some(s) = temps(readings, chip).next() {
            return Some(s.value);
        }
    }
    None
}

/// Temperatura mais alta entre os discos (NVMe e drivetemp).
pub fn disk_temperatures(readings: &[SensorReading]) -> Vec<(String, f64)> {
    readings
        .iter()
        .filter(|s| s.kind == SensorKind::Temp && (s.chip.starts_with("nvme") || s.chip.starts_with("drivetemp")) && (s.label == "Composite" || s.label == "temp1"))
        .map(|s| (s.chip_display.clone(), s.value))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Carga da CPU
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStat {
    /// `None` = linha agregada ("cpu").
    pub id: Option<usize>,
    pub busy: u64,
    pub total: u64,
}

/// Interpreta /proc/stat.
pub fn parse_proc_stat(text: &str) -> Vec<CoreStat> {
    text.lines()
        .filter(|l| l.starts_with("cpu"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
            if v.len() < 4 {
                return None;
            }
            let id = if name == "cpu" { None } else { Some(name[3..].parse().ok()?) };
            // user nice system idle iowait irq softirq steal (guest já está dentro de user)
            let idle = v[3] + v.get(4).copied().unwrap_or(0);
            let total: u64 = v.iter().take(8).sum();
            Some(CoreStat { id, busy: total.saturating_sub(idle), total })
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuLoad {
    /// Uso total (0 a 100).
    pub total: f64,
    /// Uso de cada processador lógico (0 a 100).
    pub per_core: Vec<f64>,
}

impl CpuLoad {
    /// Processador lógico mais ocupado: (índice, uso).
    pub fn busiest(&self) -> Option<(usize, f64)> {
        self.per_core.iter().copied().enumerate().max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// Mede a carga comparando duas leituras de /proc/stat.
pub struct CpuSampler {
    path: std::path::PathBuf,
    prev: Vec<CoreStat>,
}

impl CpuSampler {
    pub fn new(r: &Roots) -> Self {
        let path = r.proc.join("stat");
        let prev = parse_proc_stat(&fs::read_to_string(&path).unwrap_or_default());
        CpuSampler { path, prev }
    }

    /// Carga desde a última chamada (ou desde `new`).
    pub fn sample(&mut self) -> CpuLoad {
        let cur = parse_proc_stat(&fs::read_to_string(&self.path).unwrap_or_default());
        let pct = |a: &CoreStat, b: &CoreStat| {
            let dt = b.total.saturating_sub(a.total);
            if dt == 0 { 0.0 } else { (b.busy.saturating_sub(a.busy)) as f64 / dt as f64 * 100.0 }
        };
        let mut load = CpuLoad::default();
        for c in &cur {
            let Some(p) = self.prev.iter().find(|p| p.id == c.id) else { continue };
            let v = pct(p, c).clamp(0.0, 100.0);
            match c.id {
                None => load.total = v,
                Some(_) => load.per_core.push(v),
            }
        }
        self.prev = cur;
        load
    }
}

/// Clock atual de cada processador lógico, em MHz.
pub fn cpu_clocks(r: &Roots) -> Vec<f64> {
    let base = r.sys.join("devices/system/cpu");
    let mut v: Vec<(u32, f64)> = Vec::new();
    for n in dir_names(&base) {
        let Some(id) = n.strip_prefix("cpu").and_then(|d| d.parse::<u32>().ok()) else { continue };
        let f = read_num::<f64>(base.join(&n).join("cpufreq/scaling_cur_freq")).or_else(|| read_num::<f64>(base.join(&n).join("cpufreq/cpuinfo_cur_freq")));
        if let Some(khz) = f {
            v.push((id, khz / 1000.0));
        }
    }
    if v.is_empty() {
        // Sem cpufreq (VMs, alguns ARM): usa "cpu MHz" do /proc/cpuinfo
        let text = fs::read_to_string(r.proc.join("cpuinfo")).unwrap_or_default();
        for (i, l) in text.lines().filter(|l| l.starts_with("cpu MHz")).enumerate() {
            if let Some(m) = l.split(':').nth(1).and_then(|x| x.trim().parse::<f64>().ok()) {
                v.push((i as u32, m));
            }
        }
    }
    v.sort_by_key(|(id, _)| *id);
    v.into_iter().map(|(_, m)| m).collect()
}

// ---------------------------------------------------------------------------------------------
// Consumo da CPU (RAPL)
// ---------------------------------------------------------------------------------------------

/// Watts a partir de duas leituras de energia em µJ, tratando a volta do contador.
pub fn rapl_watts(prev_uj: u64, cur_uj: u64, max_range_uj: u64, dt_secs: f64) -> Option<f64> {
    if dt_secs <= 0.0 {
        return None;
    }
    let delta = if cur_uj >= prev_uj {
        cur_uj - prev_uj
    } else if max_range_uj > prev_uj {
        max_range_uj - prev_uj + cur_uj
    } else {
        return None;
    };
    Some(delta as f64 / 1_000_000.0 / dt_secs)
}

/// Lê o consumo do pacote da CPU pelo RAPL. Desde o kernel 5.10 o `energy_uj` só é legível por root.
pub struct RaplSampler {
    energy_path: Option<std::path::PathBuf>,
    max_range: u64,
    last: Option<(u64, Instant)>,
    /// Motivo de não haver leitura, para mostrar ao usuário.
    pub note: Option<String>,
}

impl RaplSampler {
    pub fn new(r: &Roots) -> Self {
        let base = r.sys.join("class/powercap");
        let zone = dir_names(&base).into_iter().find(|n| {
            n.starts_with("intel-rapl:") && n.matches(':').count() == 1 && read_trim(base.join(n).join("name")).is_some_and(|nm| nm.starts_with("package"))
        });
        let Some(zone) = zone else {
            return RaplSampler { energy_path: None, max_range: 0, last: None, note: Some("a CPU não expõe o RAPL (powercap)".into()) };
        };
        let energy = base.join(&zone).join("energy_uj");
        let max_range = read_num(base.join(&zone).join("max_energy_range_uj")).unwrap_or(0);
        let mut s = RaplSampler { energy_path: Some(energy.clone()), max_range, last: None, note: None };
        match fs::read_to_string(&energy) {
            Ok(t) => s.last = t.trim().parse().ok().map(|v| (v, Instant::now())),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                s.note = Some("o consumo da CPU (RAPL) só pode ser lido como root: rode com sudo".into());
            }
            Err(e) => s.note = Some(format!("não consegui ler o RAPL: {e}")),
        }
        s
    }

    /// Consumo médio em watts desde a última chamada.
    pub fn sample(&mut self) -> Option<f64> {
        let path = self.energy_path.as_ref()?;
        let cur: u64 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
        let now = Instant::now();
        let w = self.last.and_then(|(p, t)| rapl_watts(p, cur, self.max_range, now.duration_since(t).as_secs_f64()));
        self.last = Some((cur, now));
        w
    }
}

/// Agrupa leituras por chip, mantendo a ordem.
pub fn group_by_chip(readings: &[SensorReading]) -> BTreeMap<String, Vec<&SensorReading>> {
    let mut m: BTreeMap<String, Vec<&SensorReading>> = BTreeMap::new();
    for r in readings {
        m.entry(r.chip_display.clone()).or_default().push(r);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn splits_hwmon_attribute_names() {
        assert_eq!(split_attr("temp1_input"), Some(("temp", 1, "input")));
        assert_eq!(split_attr("fan12_input"), Some(("fan", 12, "input")));
        assert_eq!(split_attr("pwm2"), Some(("pwm", 2, "")));
        assert_eq!(split_attr("in0_label"), Some(("in", 0, "label")));
        assert_eq!(split_attr("name"), None);
    }

    fn fake_hwmon(root: &Path) {
        write(root, "sys/class/hwmon/hwmon0/name", "k10temp\n");
        write(root, "sys/class/hwmon/hwmon0/temp1_input", "61250\n");
        write(root, "sys/class/hwmon/hwmon0/temp1_label", "Tctl\n");
        write(root, "sys/class/hwmon/hwmon0/temp3_input", "55000\n");
        write(root, "sys/class/hwmon/hwmon0/temp3_label", "Tccd1\n");
        write(root, "sys/class/hwmon/hwmon1/name", "nvme\n");
        write(root, "sys/class/hwmon/hwmon1/device/model", "Samsung SSD 990 PRO 1TB\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_input", "41850\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_label", "Composite\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_max", "81850\n");
        write(root, "sys/class/hwmon/hwmon1/temp1_crit", "84850\n");
        write(root, "sys/class/hwmon/hwmon2/name", "amdgpu\n");
        write(root, "sys/class/hwmon/hwmon2/temp1_input", "48000\n");
        write(root, "sys/class/hwmon/hwmon2/temp2_input", "62000\n");
        write(root, "sys/class/hwmon/hwmon2/temp2_label", "junction\n");
        write(root, "sys/class/hwmon/hwmon2/power1_average", "105000000\n");
        write(root, "sys/class/hwmon/hwmon2/power1_input", "104000000\n");
        write(root, "sys/class/hwmon/hwmon2/fan1_input", "1450\n");
        write(root, "sys/class/hwmon/hwmon2/pwm1", "128\n");
        write(root, "sys/class/hwmon/hwmon2/in0_input", "1075\n");
        write(root, "sys/class/hwmon/hwmon2/in0_label", "vddgfx\n");
        write(root, "sys/class/hwmon/hwmon2/freq1_input", "2500000000\n");
        write(root, "sys/class/thermal/thermal_zone0/type", "acpitz\n");
        write(root, "sys/class/thermal/thermal_zone0/temp", "27800\n");
        write(root, "sys/class/thermal/thermal_zone1/type", "x86_pkg_temp\n");
        write(root, "sys/class/thermal/thermal_zone1/temp", "50000\n");
    }

    #[test]
    fn reads_hwmon_chips_with_units_and_limits() {
        let d = tempfile::tempdir().unwrap();
        fake_hwmon(d.path());
        let v = read_all(&Roots::under(d.path()));
        let get = |chip: &str, label: &str| v.iter().find(|s| s.chip == chip && s.label == label).unwrap_or_else(|| panic!("falta {chip}/{label}"));
        assert!((get("k10temp", "Tctl").value - 61.25).abs() < 1e-9);
        let nvme = get("nvme", "Composite");
        assert!((nvme.value - 41.85).abs() < 1e-9);
        assert_eq!(nvme.max, Some(81.85));
        assert_eq!(nvme.crit, Some(84.85));
        assert_eq!(nvme.chip_display, "NVMe Samsung SSD 990 PRO 1TB");
        assert!((get("amdgpu", "temp1").value - 48.0).abs() < 1e-9);
        assert!((get("amdgpu", "fan1").value - 1450.0).abs() < 1e-9);
        assert!((get("amdgpu", "pwm1").value - 50.2).abs() < 0.1);
        assert!((get("amdgpu", "vddgfx").value - 1.075).abs() < 1e-9);
        assert!((get("amdgpu", "freq1").value - 2500.0).abs() < 1e-9);
        // average e input do mesmo índice: só um aparece
        let power: Vec<_> = v.iter().filter(|s| s.chip == "amdgpu" && s.kind == SensorKind::Power).collect();
        assert_eq!(power.len(), 1);
        assert!((power[0].value - 105.0).abs() < 1e-9);
        // zonas térmicas: acpitz e x86_pkg_temp aparecem (não há chip hwmon com esses nomes)
        assert!(v.iter().any(|s| s.chip == "thermal:acpitz"));
        assert!(v.iter().any(|s| s.chip == "thermal:x86_pkg_temp"));
    }

    #[test]
    fn picks_cpu_temperature_by_preference() {
        let d = tempfile::tempdir().unwrap();
        fake_hwmon(d.path());
        let v = read_all(&Roots::under(d.path()));
        assert_eq!(cpu_temperature(&v), Some(61.25));
        // Intel: Package id 0 tem prioridade sobre os núcleos
        let d2 = tempfile::tempdir().unwrap();
        write(d2.path(), "sys/class/hwmon/hwmon0/name", "coretemp\n");
        write(d2.path(), "sys/class/hwmon/hwmon0/temp1_input", "70000\n");
        write(d2.path(), "sys/class/hwmon/hwmon0/temp1_label", "Package id 0\n");
        write(d2.path(), "sys/class/hwmon/hwmon0/temp2_input", "80000\n");
        write(d2.path(), "sys/class/hwmon/hwmon0/temp2_label", "Core 0\n");
        assert_eq!(cpu_temperature(&read_all(&Roots::under(d2.path()))), Some(70.0));
        // sem nada reconhecível
        assert_eq!(cpu_temperature(&[]), None);
    }

    #[test]
    fn lists_disk_temperatures() {
        let d = tempfile::tempdir().unwrap();
        fake_hwmon(d.path());
        let t = disk_temperatures(&read_all(&Roots::under(d.path())));
        assert_eq!(t.len(), 1);
        assert!(t[0].0.contains("990 PRO"));
    }

    #[test]
    fn parses_proc_stat_and_computes_load() {
        let a = "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0 0 0\nintr 12345\n";
        let b = "cpu  225 0 125 850 0 0 0 0 0 0\ncpu0 150 0 50 400 0 0 0 0 0 0\ncpu1 75 0 75 450 0 0 0 0 0 0\nintr 99999\n";
        let (pa, pb) = (parse_proc_stat(a), parse_proc_stat(b));
        assert_eq!(pa.len(), 3);
        assert_eq!(pa[0], CoreStat { id: None, busy: 200, total: 1000 });
        assert_eq!(pa[1].id, Some(0));
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "proc/stat", a);
        let mut s = CpuSampler::new(&Roots::under(d.path()));
        write(d.path(), "proc/stat", b);
        let l = s.sample();
        // total: busy +150 de +200 => 75%; núcleo 0 100% ocupado, núcleo 1 em 50%
        assert!((l.total - 75.0).abs() < 1e-9, "{}", l.total);
        assert_eq!(l.per_core.len(), 2);
        assert!((l.per_core[0] - 100.0).abs() < 1e-9);
        assert!((l.per_core[1] - 50.0).abs() < 1e-9);
        assert_eq!(l.busiest(), Some((0, 100.0)));
        let _ = pb;
    }

    #[test]
    fn iowait_counts_as_idle() {
        let s = parse_proc_stat("cpu 10 0 10 70 10 0 0 0\n");
        assert_eq!(s[0], CoreStat { id: None, busy: 20, total: 100 });
    }

    #[test]
    fn reads_cpu_clocks_from_cpufreq_or_cpuinfo() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq", "3600000\n");
        write(d.path(), "sys/devices/system/cpu/cpu1/cpufreq/scaling_cur_freq", "4200000\n");
        write(d.path(), "sys/devices/system/cpu/cpu10/cpufreq/scaling_cur_freq", "800000\n");
        assert_eq!(cpu_clocks(&Roots::under(d.path())), [3600.0, 4200.0, 800.0]);
        let d2 = tempfile::tempdir().unwrap();
        write(d2.path(), "proc/cpuinfo", "processor : 0\ncpu MHz\t\t: 2999.998\n\nprocessor : 1\ncpu MHz\t\t: 1500.000\n");
        assert_eq!(cpu_clocks(&Roots::under(d2.path())), [2999.998, 1500.0]);
    }

    #[test]
    fn rapl_handles_counter_wraparound() {
        assert_eq!(rapl_watts(1_000_000, 11_000_000, u64::MAX, 2.0), Some(5.0));
        // contador deu a volta: max 100 J; passou de 99 J para 1 J => 2 J em 1 s
        assert_eq!(rapl_watts(99_000_000, 1_000_000, 100_000_000, 1.0), Some(2.0));
        assert_eq!(rapl_watts(5, 3, 0, 1.0), None);
        assert_eq!(rapl_watts(1, 2, 10, 0.0), None);
    }

    #[test]
    fn rapl_sampler_reads_package_zone() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "sys/class/powercap/intel-rapl:0/name", "package-0\n");
        write(d.path(), "sys/class/powercap/intel-rapl:0/energy_uj", "1000000\n");
        write(d.path(), "sys/class/powercap/intel-rapl:0/max_energy_range_uj", "262143328850\n");
        write(d.path(), "sys/class/powercap/intel-rapl:0:0/name", "core\n");
        let mut s = RaplSampler::new(&Roots::under(d.path()));
        assert!(s.note.is_none());
        // 10 J a mais depois de ~50 ms: o consumo calculado tem de ser positivo e finito
        std::thread::sleep(std::time::Duration::from_millis(50));
        write(d.path(), "sys/class/powercap/intel-rapl:0/energy_uj", "11000000\n");
        let w = s.sample().expect("deve haver leitura");
        assert!(w > 10.0 && w.is_finite(), "w = {w}");
        // a zona "core" (intel-rapl:0:0) não é o pacote e não pode ser escolhida
        let only_sub = tempfile::tempdir().unwrap();
        write(only_sub.path(), "sys/class/powercap/intel-rapl:0:0/name", "core\n");
        write(only_sub.path(), "sys/class/powercap/intel-rapl:0:0/energy_uj", "5\n");
        assert!(RaplSampler::new(&Roots::under(only_sub.path())).note.is_some());
        let none = RaplSampler::new(&Roots::under(tempfile::tempdir().unwrap().path()));
        assert!(none.note.is_some());
    }
}
