//! Armazenamento: discos físicos (/sys/block), pontos de montagem com uso, e saúde S.M.A.R.T. (via `smartctl -j`).
//!
//! O veredito de saúde segue os limites do projeto Windows (desgaste ≥ 80% atenção / ≥ 95% crítico,
//! temperatura SSD 70/85 °C e HDD 55/65 °C, falha prevista = crítico), com os atributos ATA e NVMe que o `smartctl` expõe.

use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::sensors;
use crate::util::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Verdict {
    Good,
    Warning,
    Critical,
    Unknown,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Good => "SAUDÁVEL",
            Verdict::Warning => "ATENÇÃO",
            Verdict::Critical => "CRÍTICO",
            Verdict::Unknown => "SEM DADOS",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// S.M.A.R.T.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct SmartData {
    pub protocol: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub capacity_bytes: Option<u64>,
    /// 0 = SSD, >0 = rotações por minuto (HDD).
    pub rotation_rate: Option<u32>,
    /// `smart_status.passed`: `false` significa que o disco prevê falha.
    pub passed: Option<bool>,
    pub temp: Option<f64>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    /// Desgaste do SSD (0 a 100).
    pub wear_pct: Option<f64>,
    pub spare_pct: Option<f64>,
    pub spare_threshold: Option<f64>,
    pub critical_warning: Option<u64>,
    pub media_errors: Option<u64>,
    pub error_log_entries: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub written_bytes: Option<u64>,
    pub read_bytes: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub offline_uncorrectable: Option<u64>,
    pub reported_uncorrect: Option<u64>,
    pub crc_errors: Option<u64>,
    /// Mensagens do próprio smartctl (permissão negada, dispositivo desconhecido...).
    pub messages: Vec<String>,
    pub exit_status: Option<i64>,
}

impl SmartData {
    /// O smartctl trouxe alguma leitura de saúde?
    pub fn has_data(&self) -> bool {
        self.passed.is_some()
            || self.temp.is_some()
            || self.wear_pct.is_some()
            || self.power_on_hours.is_some()
            || self.reallocated.is_some()
            || self.media_errors.is_some()
    }

    pub fn is_ssd(&self) -> bool {
        self.protocol.eq_ignore_ascii_case("nvme") || self.rotation_rate == Some(0)
    }
}

fn u64_of(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

/// Interpreta a saída de `smartctl -a -j`.
pub fn parse_smartctl_json(text: &str) -> Result<SmartData, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("saída do smartctl inválida: {e}"))?;
    let mut d = SmartData::default();
    d.protocol = v["device"]["protocol"].as_str().unwrap_or("").to_string();
    d.model = v["model_name"].as_str().map(str::to_string);
    d.serial = v["serial_number"].as_str().map(str::to_string);
    d.firmware = v["firmware_version"].as_str().map(str::to_string);
    d.capacity_bytes = u64_of(&v["user_capacity"]["bytes"]);
    d.rotation_rate = u64_of(&v["rotation_rate"]).map(|r| r as u32);
    d.passed = v["smart_status"]["passed"].as_bool();
    d.temp = v["temperature"]["current"].as_f64();
    d.power_on_hours = u64_of(&v["power_on_time"]["hours"]);
    d.power_cycles = u64_of(&v["power_cycle_count"]);
    d.exit_status = v["smartctl"]["exit_status"].as_i64();
    if let Some(msgs) = v["smartctl"]["messages"].as_array() {
        d.messages = msgs.iter().filter_map(|m| m["string"].as_str().map(str::to_string)).collect();
    }

    // NVMe
    let n = &v["nvme_smart_health_information_log"];
    if n.is_object() {
        d.critical_warning = u64_of(&n["critical_warning"]);
        d.temp = d.temp.or_else(|| n["temperature"].as_f64());
        d.spare_pct = n["available_spare"].as_f64();
        d.spare_threshold = n["available_spare_threshold"].as_f64();
        d.wear_pct = n["percentage_used"].as_f64();
        // unidades de 1000 setores de 512 bytes
        d.read_bytes = u64_of(&n["data_units_read"]).map(|u| u.saturating_mul(512_000));
        d.written_bytes = u64_of(&n["data_units_written"]).map(|u| u.saturating_mul(512_000));
        d.power_cycles = d.power_cycles.or_else(|| u64_of(&n["power_cycles"]));
        d.power_on_hours = d.power_on_hours.or_else(|| u64_of(&n["power_on_hours"]));
        d.unsafe_shutdowns = u64_of(&n["unsafe_shutdowns"]);
        d.media_errors = u64_of(&n["media_errors"]);
        d.error_log_entries = u64_of(&n["num_err_log_entries"]);
    }

    // ATA
    if let Some(table) = v["ata_smart_attributes"]["table"].as_array() {
        let lba = u64_of(&v["logical_block_size"]).unwrap_or(512);
        let mut life_left: Option<f64> = None;
        for a in table {
            let id = a["id"].as_u64().unwrap_or(0);
            let raw = u64_of(&a["raw"]["value"]);
            let norm = a["value"].as_f64();
            match id {
                5 => d.reallocated = raw,
                12 => d.power_cycles = d.power_cycles.or(raw),
                187 => d.reported_uncorrect = raw,
                197 => d.pending = raw,
                198 => d.offline_uncorrectable = raw,
                199 => d.crc_errors = raw,
                // vida restante normalizada (100 = novo)
                169 | 202 | 231 | 233 => life_left = life_left.or(norm),
                177 => d.wear_pct = d.wear_pct.or(norm.map(|n| (100.0 - n).clamp(0.0, 100.0))),
                241 => d.written_bytes = d.written_bytes.or(raw.map(|r| r.saturating_mul(lba))),
                242 => d.read_bytes = d.read_bytes.or(raw.map(|r| r.saturating_mul(lba))),
                _ => {}
            }
        }
        if d.wear_pct.is_none() {
            d.wear_pct = life_left.map(|l| (100.0 - l).clamp(0.0, 100.0));
        }
    }
    Ok(d)
}

/// Avalia a saúde de um disco. Devolve o veredito e os motivos (críticos primeiro).
pub fn assess(d: &SmartData) -> (Verdict, Vec<String>) {
    let mut crit: Vec<String> = Vec::new();
    let mut warn: Vec<String> = Vec::new();
    let ssd = d.is_ssd();

    if d.passed == Some(false) {
        crit.push("O próprio disco avisa (S.M.A.R.T.) que deve falhar em breve. Faça backup agora.".into());
    }
    if let Some(cw) = d.critical_warning.filter(|c| *c != 0) {
        let mut bits = Vec::new();
        for (bit, t) in [(0x01, "reserva de células abaixo do limite"), (0x02, "temperatura fora do limite"), (0x04, "confiabilidade degradada"), (0x08, "disco em modo somente leitura"), (0x10, "memória volátil de backup com falha")] {
            if cw & bit != 0 {
                bits.push(t);
            }
        }
        crit.push(format!("O NVMe reporta alerta crítico: {}.", bits.join(", ")));
    }
    if let (Some(s), Some(t)) = (d.spare_pct, d.spare_threshold) {
        if s < t {
            crit.push(format!("A reserva de células do SSD ({s:.0}%) está abaixo do limite do fabricante ({t:.0}%)."));
        }
    }
    if let Some(w) = d.wear_pct {
        if w >= 95.0 {
            crit.push(format!("Desgaste de {w:.0}%: o SSD está no fim da vida útil. Troque e faça backup."));
        } else if w >= 80.0 {
            warn.push(format!("Desgaste de {w:.0}%: o SSD já gastou boa parte da vida útil. Planeje a troca."));
        }
    }
    if let Some(t) = d.temp {
        let (ok_max, crit_max) = if ssd { (70.0, 85.0) } else { (55.0, 65.0) };
        if t >= crit_max {
            crit.push(format!("Temperatura de {t:.0} °C é muito alta para este disco."));
        } else if t >= ok_max {
            warn.push(format!("Temperatura de {t:.0} °C está alta. Melhore a ventilação."));
        }
    }
    if let Some(r) = d.reallocated.filter(|r| *r > 0) {
        if r >= 50 {
            crit.push(format!("{r} setores realocados: a superfície do disco está falhando. Faça backup e troque."));
        } else {
            warn.push(format!("{r} setores realocados. Acompanhe: se o número crescer, troque o disco."));
        }
    }
    let bad_sectors = d.pending.unwrap_or(0) + d.offline_uncorrectable.unwrap_or(0);
    if bad_sectors >= 10 {
        crit.push(format!("{bad_sectors} setores pendentes/não corrigíveis. Faça backup agora."));
    } else if bad_sectors > 0 {
        warn.push(format!("{bad_sectors} setor(es) pendente(s) ou não corrigível(is). Faça backup por precaução."));
    }
    if let Some(e) = d.reported_uncorrect.filter(|e| *e > 0) {
        warn.push(format!("{e} erro(s) de leitura não corrigidos reportados pelo disco."));
    }
    if let Some(e) = d.media_errors.filter(|e| *e > 0) {
        warn.push(format!("{e} erro(s) de integridade de mídia no NVMe."));
    }
    if let Some(e) = d.crc_errors.filter(|e| *e > 0) {
        warn.push(format!("{e} erro(s) de CRC na interface SATA: costuma ser cabo ou conector ruim."));
    }

    let verdict = if !crit.is_empty() {
        Verdict::Critical
    } else if !warn.is_empty() {
        Verdict::Warning
    } else if d.has_data() {
        Verdict::Good
    } else {
        Verdict::Unknown
    };
    let mut reasons = crit;
    reasons.extend(warn);
    if reasons.is_empty() {
        reasons.push(if verdict == Verdict::Good {
            "Nenhum problema encontrado nas leituras disponíveis.".into()
        } else {
            "Este disco não informa dados de saúde (comum em pendrives e alguns HDs externos).".into()
        });
    }
    (verdict, reasons)
}

/// Linhas "nome: valor" para mostrar e exportar.
pub fn metrics(d: &SmartData) -> Vec<(String, String)> {
    let mut m = Vec::new();
    let mut add = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            m.push((k.to_string(), v));
        }
    };
    add("Temperatura", d.temp.map(|t| format!("{t:.0} °C")));
    if let Some(w) = d.wear_pct {
        add("Desgaste", Some(format!("{w:.0}%")));
        add("Vida útil restante", Some(format!("{:.0}%", (100.0 - w).max(0.0))));
    }
    add("Reserva de células", d.spare_pct.map(|s| format!("{s:.0}%")));
    add("Horas ligado", d.power_on_hours.map(|h| format!("{h} h ({:.0} dias)", h as f64 / 24.0)));
    add("Ciclos liga/desliga", d.power_cycles.map(|c| c.to_string()));
    add("Desligamentos inesperados", d.unsafe_shutdowns.map(|c| c.to_string()));
    add("Setores realocados", d.reallocated.map(|c| c.to_string()));
    add("Setores pendentes", d.pending.map(|c| c.to_string()));
    add("Não corrigíveis (offline)", d.offline_uncorrectable.map(|c| c.to_string()));
    add("Erros de mídia (NVMe)", d.media_errors.map(|c| c.to_string()));
    add("Erros de CRC (cabo)", d.crc_errors.map(|c| c.to_string()));
    add("Total gravado", d.written_bytes.filter(|b| *b > 0).map(fmt_bytes_si));
    add("Total lido", d.read_bytes.filter(|b| *b > 0).map(fmt_bytes_si));
    add("Falha prevista (S.M.A.R.T.)", d.passed.map(|p| if p { "Não".to_string() } else { "SIM".to_string() }));
    add("Firmware", d.firmware.clone());
    m
}

/// Consulta o smartctl de um disco. Tenta sem privilégios e, se faltar permissão, com sudo (quando disponível).
pub fn read_smart(dev: &str, usb: bool) -> Result<SmartData, String> {
    if !have("smartctl") {
        return Err("instale o smartmontools (comando smartctl) para ver a saúde do disco".into());
    }
    let path = format!("/dev/{dev}");
    let attempt = |extra: &[&str]| -> Result<SmartData, String> {
        let mut args: Vec<&str> = vec!["-a", "-j"];
        args.extend_from_slice(extra);
        args.push(&path);
        let o = run_maybe_priv("smartctl", &args, Duration::from_secs(15)).ok_or("não consegui executar o smartctl")?;
        if o.timed_out {
            return Err("o smartctl não respondeu a tempo".into());
        }
        if o.stdout.trim().is_empty() {
            return Err(if o.combined().to_lowercase().contains("permission") { "sem permissão para ler o S.M.A.R.T.: rode com sudo".into() } else { o.last_line() });
        }
        parse_smartctl_json(&o.stdout)
    };
    let mut d = attempt(&[])?;
    if !d.has_data() && usb {
        // Pontes USB-SATA costumam precisar do modo SAT
        if let Ok(d2) = attempt(&["-d", "sat"]) {
            if d2.has_data() {
                d = d2;
            }
        }
    }
    if !d.has_data() {
        let why = d.messages.iter().find(|m| m.to_lowercase().contains("permission")).cloned();
        if let Some(w) = why {
            return Err(format!("sem permissão para ler o S.M.A.R.T. ({w}). Rode com sudo."));
        }
    }
    Ok(d)
}

// ---------------------------------------------------------------------------------------------
// Discos físicos
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Disk {
    /// Nome em /dev, como "nvme0n1" ou "sda".
    pub name: String,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub size: u64,
    pub rotational: bool,
    pub removable: bool,
    /// "NVMe", "SATA", "USB", "SD/eMMC", "VirtIO"...
    pub bus: String,
    pub scheduler: Option<String>,
    pub partitions: Vec<String>,
    pub mountpoints: Vec<String>,
    /// Temperatura vinda do hwmon (sem precisar de privilégios).
    pub temp: Option<f64>,
    pub smart: Option<SmartData>,
    pub smart_note: Option<String>,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

impl Disk {
    pub fn kind(&self) -> &'static str {
        if self.bus == "NVMe" || (!self.rotational && self.bus != "SD/eMMC" && self.bus != "USB") {
            "SSD"
        } else if self.rotational {
            "HDD"
        } else {
            "Flash"
        }
    }

    pub fn title(&self) -> String {
        if self.model.is_empty() { self.name.clone() } else { self.model.clone() }
    }
}

/// Barramento a partir do nome do dispositivo e do destino do link em /sys/block.
pub fn bus_from_path(name: &str, link: &str) -> &'static str {
    if name.starts_with("nvme") {
        "NVMe"
    } else if name.starts_with("mmcblk") {
        "SD/eMMC"
    } else if name.starts_with("vd") || link.contains("/virtio") {
        "VirtIO"
    } else if name.starts_with("xvd") {
        "Xen"
    } else if link.contains("/usb") {
        "USB"
    } else if link.contains("/ata") {
        "SATA"
    } else if name.starts_with("sd") {
        "SCSI/SATA"
    } else {
        "Outro"
    }
}

fn is_physical_candidate(name: &str) -> bool {
    !["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd", "rbd"].iter().any(|p| name.starts_with(p))
}

/// Disco físico de uma partição ou dispositivo virtual (LVM/LUKS/RAID).
pub fn parent_disk(r: &Roots, dev: &str) -> Option<String> {
    fn go(r: &Roots, dev: &str, depth: u8) -> Option<String> {
        if depth > 5 {
            return None;
        }
        let block = r.sys.join("block");
        if block.join(dev).is_dir() {
            // dispositivo inteiro: se for dm/md, procura o disco que está por baixo
            if dev.starts_with("dm-") || dev.starts_with("md") {
                let slaves = dir_names(block.join(dev).join("slaves"));
                return slaves.first().and_then(|s| go(r, s, depth + 1));
            }
            return Some(dev.to_string());
        }
        // partição: nvme0n1p2 / mmcblk0p1 -> nvme0n1 / mmcblk0 ; sda1 -> sda
        let cut = if let Some(pos) = dev.rfind('p').filter(|p| *p > 0 && dev[*p + 1..].chars().all(|c| c.is_ascii_digit()) && dev.len() > *p + 1 && dev[..*p].ends_with(|c: char| c.is_ascii_digit())) {
            Some(&dev[..pos])
        } else {
            let t = dev.trim_end_matches(|c: char| c.is_ascii_digit());
            (t.len() < dev.len() && !t.is_empty()).then_some(t)
        };
        let parent = cut?;
        if block.join(parent).is_dir() {
            return go(r, parent, depth + 1);
        }
        // via sysfs: /sys/class/block/<part>/../
        let real = fs::canonicalize(r.sys.join("class/block").join(dev)).ok()?;
        let p = real.parent()?.file_name()?.to_string_lossy().into_owned();
        block.join(&p).is_dir().then_some(p)
    }
    go(r, dev, 0)
}

#[derive(Debug, Clone, Serialize)]
pub struct Mount {
    pub source: String,
    pub mountpoint: String,
    pub fstype: String,
    pub total: u64,
    pub used: u64,
    pub avail: u64,
    /// Disco físico onde está (quando dá para descobrir).
    pub disk: Option<String>,
}

impl Mount {
    pub fn used_pct(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.used as f64 / self.total as f64 * 100.0 }
    }
}

const REAL_FS: &[&str] = &["ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "zfs", "reiserfs", "jfs", "nfs", "nfs4", "cifs", "smb3", "bcachefs", "hfsplus", "ufs"];

/// Decodifica as sequências octais de /proc/mounts ("\040" = espaço).
pub fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 0 && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Espaço total/livre de um ponto de montagem (statvfs).
pub fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
    let c = CString::new(path).ok()?;
    // SAFETY: `c` é válido e `st` é inicializado pela chamada quando ela retorna 0.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        let fr = st.f_frsize as u64;
        let total = st.f_blocks as u64 * fr;
        let free = st.f_bfree as u64 * fr;
        let avail = st.f_bavail as u64 * fr;
        Some((total, total.saturating_sub(free), avail))
    }
}

/// Pontos de montagem de sistemas de arquivos reais, com uso e disco físico.
pub fn collect_mounts(r: &Roots) -> Vec<Mount> {
    let text = fs::read_to_string(r.proc.join("mounts")).unwrap_or_default();
    let mut v: Vec<Mount> = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 || !REAL_FS.contains(&f[2]) {
            continue;
        }
        let (source, mp) = (unescape_mount(f[0]), unescape_mount(f[1]));
        if ["/var/lib/docker", "/var/snap", "/snap/", "/var/lib/flatpak/runtime", "/run/user/", "/proc", "/sys"].iter().any(|p| mp.starts_with(p)) {
            continue;
        }
        // subvolumes btrfs: o mesmo dispositivo em vários pontos mostra o mesmo espaço
        if v.iter().any(|m| m.source == source && m.fstype == f[2] && m.mountpoint.len() <= mp.len() && f[2] == "btrfs" && !mp.starts_with("/home")) {
            continue;
        }
        let Some((total, used, avail)) = statvfs(&mp) else { continue };
        if total == 0 {
            continue;
        }
        let disk = source.strip_prefix("/dev/").and_then(|d| {
            let real = fs::canonicalize(r.dev.join(d)).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| d.rsplit('/').next().unwrap_or(d).to_string());
            parent_disk(r, &real)
        });
        v.push(Mount { source, mountpoint: mp, fstype: f[2].to_string(), total, used, avail, disk });
    }
    v.sort_by(|a, b| a.mountpoint.cmp(&b.mountpoint));
    v
}

fn dev_attr(dir: &Path, name: &str) -> String {
    read_trim(dir.join(name)).unwrap_or_default()
}

/// Lista os discos físicos. `with_smart` consulta o smartctl de cada um (mais lento, pode precisar de sudo).
pub fn collect_disks(r: &Roots, with_smart: bool) -> Vec<Disk> {
    let block = r.sys.join("block");
    let mounts = collect_mounts(r);
    let temps = sensors::disk_temperatures(&sensors::read_all(r));
    let mut disks = Vec::new();
    for name in dir_names(&block).into_iter().filter(|n| is_physical_candidate(n)) {
        let d = block.join(&name);
        let size: u64 = read_num::<u64>(d.join("size")).unwrap_or(0) * 512;
        if size == 0 {
            continue; // leitor de cartão vazio, por exemplo
        }
        let link = fs::read_link(&d).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let bus = bus_from_path(&name, &link).to_string();
        let model = {
            let m = dev_attr(&d, "device/model");
            if m.is_empty() { dev_attr(&d, "device/name") } else { m }
        };
        let vendor = dev_attr(&d, "device/vendor");
        // Em dispositivos virtio o "vendor" é só um código (0x1af4) e não há modelo: nesses casos não vira título.
        let vendor_is_name = !vendor.is_empty() && vendor != "ATA" && !vendor.starts_with("0x");
        let model = if vendor_is_name && !model.is_empty() && !model.starts_with(&vendor) { format!("{vendor} {model}") } else { model };
        let firmware = {
            let f = dev_attr(&d, "device/firmware_rev");
            if f.is_empty() { dev_attr(&d, "device/rev") } else { f }
        };
        let partitions: Vec<String> = dir_names(&d).into_iter().filter(|p| p.starts_with(&name) && d.join(p).join("partition").exists()).collect();
        let my_mounts: Vec<&Mount> = mounts.iter().filter(|m| m.disk.as_deref() == Some(name.as_str())).collect();
        let temp = temps.iter().find(|(disp, _)| !model.is_empty() && disp.contains(model.trim())).map(|(_, t)| *t);
        let mut disk = Disk {
            name: name.clone(),
            model,
            serial: dev_attr(&d, "device/serial"),
            firmware,
            size,
            rotational: dev_attr(&d, "queue/rotational") == "1",
            removable: dev_attr(&d, "removable") == "1",
            bus,
            scheduler: read_trim(d.join("queue/scheduler")).map(|s| s.split('[').nth(1).and_then(|x| x.split(']').next()).unwrap_or(&s).to_string()),
            partitions,
            mountpoints: my_mounts.iter().map(|m| m.mountpoint.clone()).collect(),
            temp,
            smart: None,
            smart_note: None,
            verdict: Verdict::Unknown,
            reasons: Vec::new(),
        };
        if with_smart {
            match read_smart(&name, disk.bus == "USB") {
                Ok(s) => {
                    let (v, reasons) = assess(&s);
                    if disk.model.is_empty() {
                        disk.model = s.model.clone().unwrap_or_default();
                    }
                    if disk.serial.is_empty() {
                        disk.serial = s.serial.clone().unwrap_or_default();
                    }
                    disk.verdict = v;
                    disk.reasons = reasons;
                    disk.smart = Some(s);
                }
                Err(e) => {
                    disk.smart_note = Some(e);
                    // Pelo menos a temperatura do hwmon
                    if let Some(t) = disk.temp {
                        let d = SmartData { temp: Some(t), protocol: if disk.bus == "NVMe" { "NVMe".into() } else { String::new() }, rotation_rate: (!disk.rotational).then_some(0).or(Some(7200)), ..Default::default() };
                        let (v, reasons) = assess(&d);
                        disk.verdict = if v == Verdict::Good { Verdict::Unknown } else { v };
                        disk.reasons = reasons;
                    } else {
                        disk.reasons = vec!["Sem dados de saúde disponíveis para este disco.".into()];
                    }
                }
            }
        }
        disks.push(disk);
    }
    disks
}

/// Caminho do arquivo de configuração do kernel que descreve o disco (útil para o relatório).
pub fn sys_path(r: &Roots, name: &str) -> PathBuf {
    r.sys.join("block").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NVME_JSON: &str = r#"{
      "device": {"name": "/dev/nvme0", "protocol": "NVMe"},
      "model_name": "Samsung SSD 990 PRO 1TB", "serial_number": "S6Z1NJ0W", "firmware_version": "4B2QJXD7",
      "user_capacity": {"bytes": 1000204886016},
      "smart_status": {"passed": true},
      "temperature": {"current": 42},
      "power_on_time": {"hours": 1234},
      "power_cycle_count": 321,
      "nvme_smart_health_information_log": {
        "critical_warning": 0, "temperature": 42, "available_spare": 100, "available_spare_threshold": 10,
        "percentage_used": 3, "data_units_read": 20000000, "data_units_written": 15000000,
        "power_cycles": 321, "power_on_hours": 1234, "unsafe_shutdowns": 12, "media_errors": 0, "num_err_log_entries": 5
      },
      "smartctl": {"exit_status": 0}
    }"#;

    const ATA_JSON: &str = r#"{
      "device": {"name": "/dev/sda", "protocol": "ATA"},
      "model_name": "ST2000DM008", "serial_number": "WCC", "firmware_version": "0001",
      "rotation_rate": 7200, "logical_block_size": 512,
      "smart_status": {"passed": true},
      "temperature": {"current": 38},
      "power_on_time": {"hours": 20000},
      "ata_smart_attributes": {"table": [
        {"id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "worst": 100, "thresh": 10, "raw": {"value": 8}},
        {"id": 12, "name": "Power_Cycle_Count", "value": 100, "worst": 100, "thresh": 20, "raw": {"value": 900}},
        {"id": 197, "name": "Current_Pending_Sector", "value": 100, "worst": 100, "thresh": 0, "raw": {"value": 0}},
        {"id": 198, "name": "Offline_Uncorrectable", "value": 100, "worst": 100, "thresh": 0, "raw": {"value": 0}},
        {"id": 199, "name": "UDMA_CRC_Error_Count", "value": 200, "worst": 200, "thresh": 0, "raw": {"value": 3}},
        {"id": 241, "name": "Total_LBAs_Written", "value": 100, "worst": 100, "thresh": 0, "raw": {"value": 1000000}}
      ]},
      "smartctl": {"exit_status": 64}
    }"#;

    #[test]
    fn parses_nvme_smartctl_json() {
        let d = parse_smartctl_json(NVME_JSON).unwrap();
        assert_eq!(d.protocol, "NVMe");
        assert!(d.is_ssd());
        assert_eq!(d.temp, Some(42.0));
        assert_eq!(d.wear_pct, Some(3.0));
        assert_eq!(d.spare_pct, Some(100.0));
        assert_eq!(d.written_bytes, Some(15_000_000 * 512_000));
        assert_eq!(d.unsafe_shutdowns, Some(12));
        assert_eq!(d.power_on_hours, Some(1234));
        let (v, reasons) = assess(&d);
        assert_eq!(v, Verdict::Good);
        assert!(reasons[0].contains("Nenhum problema"));
        assert!(metrics(&d).iter().any(|(k, v)| k == "Vida útil restante" && v == "97%"));
    }

    #[test]
    fn parses_ata_smartctl_json_and_warns_about_reallocated_sectors() {
        let d = parse_smartctl_json(ATA_JSON).unwrap();
        assert!(!d.is_ssd());
        assert_eq!(d.reallocated, Some(8));
        assert_eq!(d.crc_errors, Some(3));
        assert_eq!(d.written_bytes, Some(512_000_000));
        let (v, reasons) = assess(&d);
        assert_eq!(v, Verdict::Warning);
        assert!(reasons.iter().any(|r| r.contains("8 setores realocados")));
        assert!(reasons.iter().any(|r| r.contains("CRC")));
    }

    #[test]
    fn thresholds_match_the_windows_version() {
        let ssd = |wear: f64, temp: f64| SmartData { protocol: "NVMe".into(), wear_pct: Some(wear), temp: Some(temp), ..Default::default() };
        assert_eq!(assess(&ssd(79.0, 69.0)).0, Verdict::Good);
        assert_eq!(assess(&ssd(80.0, 40.0)).0, Verdict::Warning);
        assert_eq!(assess(&ssd(95.0, 40.0)).0, Verdict::Critical);
        assert_eq!(assess(&ssd(10.0, 70.0)).0, Verdict::Warning);
        assert_eq!(assess(&ssd(10.0, 85.0)).0, Verdict::Critical);
        let hdd = |temp: f64| SmartData { protocol: "ATA".into(), rotation_rate: Some(7200), temp: Some(temp), ..Default::default() };
        assert_eq!(assess(&hdd(54.0)).0, Verdict::Good);
        assert_eq!(assess(&hdd(55.0)).0, Verdict::Warning);
        assert_eq!(assess(&hdd(65.0)).0, Verdict::Critical);
    }

    #[test]
    fn predicted_failure_is_critical_and_comes_first() {
        let d = SmartData { passed: Some(false), temp: Some(60.0), rotation_rate: Some(5400), protocol: "ATA".into(), ..Default::default() };
        let (v, reasons) = assess(&d);
        assert_eq!(v, Verdict::Critical);
        assert!(reasons[0].contains("deve falhar"));
        assert!(reasons.len() >= 2, "o aviso de temperatura também aparece");
    }

    #[test]
    fn no_data_is_unknown_not_good() {
        let (v, reasons) = assess(&SmartData::default());
        assert_eq!(v, Verdict::Unknown);
        assert!(reasons[0].contains("não informa"));
    }

    #[test]
    fn nvme_critical_warning_and_spare_below_threshold() {
        let d = SmartData { protocol: "NVMe".into(), critical_warning: Some(0x09), spare_pct: Some(5.0), spare_threshold: Some(10.0), passed: Some(true), ..Default::default() };
        let (v, reasons) = assess(&d);
        assert_eq!(v, Verdict::Critical);
        assert!(reasons[0].contains("reserva de células abaixo do limite"));
        assert!(reasons[0].contains("somente leitura"));
        assert!(reasons.iter().any(|r| r.contains("abaixo do limite do fabricante")));
    }

    #[test]
    fn ssd_wear_from_vendor_attributes() {
        let json = r#"{"device":{"protocol":"ATA"},"rotation_rate":0,"smart_status":{"passed":true},"ata_smart_attributes":{"table":[
          {"id":177,"name":"Wear_Leveling_Count","value":15,"worst":15,"thresh":0,"raw":{"value":900}}]}}"#;
        let d = parse_smartctl_json(json).unwrap();
        assert_eq!(d.wear_pct, Some(85.0));
        assert_eq!(assess(&d).0, Verdict::Warning);
        let json2 = r#"{"device":{"protocol":"ATA"},"rotation_rate":0,"smart_status":{"passed":true},"ata_smart_attributes":{"table":[
          {"id":231,"name":"SSD_Life_Left","value":4,"worst":4,"thresh":10,"raw":{"value":4}}]}}"#;
        assert_eq!(parse_smartctl_json(json2).unwrap().wear_pct, Some(96.0));
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_smartctl_json("não é json").is_err());
        let empty = parse_smartctl_json("{}").unwrap();
        assert!(!empty.has_data());
    }

    #[test]
    fn detects_bus_types() {
        assert_eq!(bus_from_path("nvme0n1", ""), "NVMe");
        assert_eq!(bus_from_path("sda", "../devices/pci0000:00/0000:00:14.0/usb2/2-1/host6/target6:0:0/6:0:0:0/block/sda"), "USB");
        assert_eq!(bus_from_path("sda", "../devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda"), "SATA");
        assert_eq!(bus_from_path("mmcblk0", ""), "SD/eMMC");
        assert_eq!(bus_from_path("vda", ""), "VirtIO");
    }

    #[test]
    fn unescapes_mount_fields() {
        assert_eq!(unescape_mount("/media/Meu\\040Disco"), "/media/Meu Disco");
        assert_eq!(unescape_mount("/mnt/a\\134b"), "/mnt/a\\b");
        assert_eq!(unescape_mount("/normal"), "/normal");
        assert_eq!(unescape_mount("/fim\\04"), "/fim\\04", "sequência incompleta é mantida");
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn fake_system(root: &Path) {
        // nvme0n1 com 2 partições, sda (HDD USB), loop0 e zram0 (ignorados)
        write(root, "sys/block/nvme0n1/size", "1953525168\n");
        write(root, "sys/block/nvme0n1/device/model", "Samsung SSD 990 PRO 1TB\n");
        write(root, "sys/block/nvme0n1/device/serial", "S6Z1NJ0W\n");
        write(root, "sys/block/nvme0n1/device/firmware_rev", "4B2QJXD7\n");
        write(root, "sys/block/nvme0n1/queue/rotational", "0\n");
        write(root, "sys/block/nvme0n1/queue/scheduler", "[none] mq-deadline\n");
        write(root, "sys/block/nvme0n1/removable", "0\n");
        write(root, "sys/block/nvme0n1/nvme0n1p1/partition", "1\n");
        write(root, "sys/block/nvme0n1/nvme0n1p2/partition", "2\n");
        write(root, "sys/block/sda/size", "976773168\n");
        write(root, "sys/block/sda/device/model", "Elements 25A3\n");
        write(root, "sys/block/sda/device/vendor", "WD\n");
        write(root, "sys/block/sda/queue/rotational", "1\n");
        write(root, "sys/block/sda/sda1/partition", "1\n");
        write(root, "sys/block/loop0/size", "100\n");
        write(root, "sys/block/zram0/size", "100\n");
        write(root, "sys/block/dm-0/size", "100\n");
        write(root, "sys/block/dm-0/slaves/nvme0n1p2/x", "");
        write(root, "sys/class/hwmon/hwmon0/name", "nvme\n");
        write(root, "sys/class/hwmon/hwmon0/device/model", "Samsung SSD 990 PRO 1TB\n");
        write(root, "sys/class/hwmon/hwmon0/temp1_input", "43000\n");
        write(root, "sys/class/hwmon/hwmon0/temp1_label", "Composite\n");
    }

    #[test]
    fn finds_parent_disks_of_partitions_and_mappers() {
        let d = tempfile::tempdir().unwrap();
        fake_system(d.path());
        let r = Roots::under(d.path());
        assert_eq!(parent_disk(&r, "nvme0n1p2").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_disk(&r, "nvme0n1").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_disk(&r, "sda1").as_deref(), Some("sda"));
        assert_eq!(parent_disk(&r, "dm-0").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_disk(&r, "inexistente3"), None);
    }

    #[test]
    fn collects_physical_disks_only() {
        let d = tempfile::tempdir().unwrap();
        fake_system(d.path());
        let r = Roots::under(d.path());
        let disks = collect_disks(&r, false);
        let names: Vec<_> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["nvme0n1", "sda"]);
        let nvme = &disks[0];
        assert_eq!(nvme.model, "Samsung SSD 990 PRO 1TB");
        assert_eq!(nvme.size, 1953525168 * 512);
        assert_eq!(nvme.bus, "NVMe");
        assert_eq!(nvme.kind(), "SSD");
        assert_eq!(nvme.scheduler.as_deref(), Some("none"));
        assert_eq!(nvme.partitions, ["nvme0n1p1", "nvme0n1p2"]);
        assert_eq!(nvme.temp, Some(43.0));
        let sda = &disks[1];
        assert_eq!(sda.model, "WD Elements 25A3");
        assert_eq!(sda.kind(), "HDD");
    }

    #[test]
    fn virtio_disk_without_model_is_not_titled_by_vendor_code() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "sys/block/vda/size", "2048\n");
        write(d.path(), "sys/block/vda/device/vendor", "0x1af4\n");
        let disks = collect_disks(&Roots::under(d.path()), false);
        assert_eq!(disks.len(), 1);
        assert!(!disks[0].model.contains("0x1af4"), "modelo: {:?}", disks[0].model);
        assert!(!disks[0].title().contains("0x1af4"), "título: {:?}", disks[0].title());
    }

    #[test]
    fn collects_mounts_with_usage() {
        let d = tempfile::tempdir().unwrap();
        fake_system(d.path());
        let mp = d.path().join("montagem real");
        fs::create_dir_all(&mp).unwrap();
        let mounts = format!(
            "proc /proc proc rw 0 0\n/dev/nvme0n1p2 {} ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n/dev/loop3 /snap/core/1 squashfs ro 0 0\n/dev/sda1 /nao/existe/aqui ext4 rw 0 0\n",
            mp.to_string_lossy().replace(' ', "\\040")
        );
        write(d.path(), "proc/mounts", &mounts);
        let r = Roots::under(d.path());
        let m = collect_mounts(&r);
        assert_eq!(m.len(), 1, "só o ext4 que existe: {m:?}");
        assert_eq!(m[0].mountpoint, mp.to_string_lossy());
        assert_eq!(m[0].disk.as_deref(), Some("nvme0n1"));
        assert!(m[0].total > 0 && m[0].total >= m[0].used);
        assert!(m[0].used_pct() <= 100.0);
        let disks = collect_disks(&r, false);
        assert_eq!(disks[0].mountpoints, [mp.to_string_lossy().to_string()]);
    }

    #[test]
    fn statvfs_of_missing_path_is_none() {
        assert!(statvfs("/caminho/que/nao/existe").is_none());
        assert!(statvfs("/").is_some());
    }
}
