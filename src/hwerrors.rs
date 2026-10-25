//! Erros de hardware registrados pelo kernel: MCE, EDAC (RAM), PCIe AER, reinício de driver de vídeo,
//! erros de E/S de disco e limitação térmica. Equivalente Linux dos eventos WHEA do Windows.
//!
//! Fonte: `journalctl -k` (ou `dmesg`). Em muitas distros ler o log do kernel exige estar nos grupos
//! `adm`/`systemd-journal` ou rodar com sudo; quando não dá para ler, o resultado informa o motivo.

use std::time::Duration;

use serde::Serialize;

use crate::util::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ErrKind {
    /// Machine Check Exception e erros reportados pela CPU.
    Cpu,
    /// EDAC / falha de página de memória.
    Memory,
    /// PCIe Advanced Error Reporting.
    Pcie,
    /// Reinício ou travamento do driver de vídeo.
    Gpu,
    /// Erros de leitura/escrita em disco ou NVMe.
    Storage,
    /// Redução de clock por temperatura.
    Thermal,
    /// Processo morto por falta de memória (não é falha de hardware).
    Oom,
}

impl ErrKind {
    pub fn title(self) -> &'static str {
        match self {
            ErrKind::Cpu => "CPU (MCE)",
            ErrKind::Memory => "Memória RAM",
            ErrKind::Pcie => "PCIe",
            ErrKind::Gpu => "Driver de vídeo",
            ErrKind::Storage => "Disco",
            ErrKind::Thermal => "Limitação térmica",
            ErrKind::Oom => "Falta de memória",
        }
    }
}

/// Classifica uma linha do log do kernel. Devolve `None` para mensagens comuns.
pub fn classify_line(msg: &str) -> Option<ErrKind> {
    let m = msg.to_lowercase();
    let has = |s: &str| m.contains(s);
    if has("out of memory: kill") || has("oom-kill:") {
        return Some(ErrKind::Oom);
    }
    if has("[hardware error]") || has("mce: ") && (has("machine check") || has("hardware error")) || has("machine check exception") {
        return Some(ErrKind::Cpu);
    }
    if has("core temperature above threshold") || has("package temperature above threshold") || has("cpu clock throttled") || has("critical temperature reached") {
        return Some(ErrKind::Thermal);
    }
    if (has("edac") && (has(" ce ") || has(" ue ") || has("correctable") || has("uncorrectable") || has("error"))) || has("memory failure:") || has("hardware memory error") {
        return Some(ErrKind::Memory);
    }
    if has("aer:") && (has("error") || has("corrected") || has("uncorrected")) || has("pcie bus error") {
        return Some(ErrKind::Pcie);
    }
    if has("nvrm: xid") || has("gpu hang") || has("gpu reset") || has("ring gfx") && has("timeout") || has("*error* ring") || has("amdgpu: gpu reset") || has("gpu fault detected") || has("nvidia-modeset: error") || has("drm_sched") && has("timeout") || has("hung_task") && has("nvidia") {
        return Some(ErrKind::Gpu);
    }
    if has("i/o error, dev") || has("buffer i/o error") || has("nvme") && (has("controller is down") || has("i/o") && has("timeout") || has("reset controller") || has("media and data integrity")) || has("blk_update_request: i/o error") || has("ata") && (has("failed command") || has("hard resetting link") && has("exception")) || has("ext4-fs error") || has("xfs") && has("metadata i/o error") || has("btrfs") && has("csum failed") {
        return Some(ErrKind::Storage);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Datas ISO 8601
// ---------------------------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Converte "2026-10-22T23:13:01-0300" / "...,123456-03:00" / "...Z" em segundos Unix.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if rest.starts_with(',') || rest.starts_with('.') {
        let end = rest[1..].find(|c: char| !c.is_ascii_digit()).map_or(rest.len(), |i| i + 1);
        rest = &rest[end..];
    }
    let offset = match rest.chars().next() {
        Some('Z') | None => 0,
        Some(sign @ ('+' | '-')) => {
            let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).take(4).collect();
            if digits.len() < 2 {
                return None;
            }
            let hh: i64 = digits[..2].parse().ok()?;
            let mm: i64 = if digits.len() >= 4 { digits[2..4].parse().ok()? } else { 0 };
            let v = hh * 3600 + mm * 60;
            if sign == '-' { -v } else { v }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset)
}

// ---------------------------------------------------------------------------------------------
// Varredura
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct HardwareErrors {
    pub cpu: u32,
    pub memory: u32,
    pub pcie: u32,
    pub gpu: u32,
    pub storage: u32,
    pub thermal: u32,
    pub oom: u32,
    /// Última ocorrência encontrada (para mostrar na tela).
    pub last_detail: Option<String>,
    /// Por que o log não pôde ser lido.
    pub error: Option<String>,
}

impl HardwareErrors {
    /// Falhas de hardware (exclui falta de memória e limitação térmica).
    pub fn total(&self) -> u32 {
        self.cpu + self.memory + self.pcie + self.gpu + self.storage
    }

    pub fn add(&mut self, kind: ErrKind) {
        match kind {
            ErrKind::Cpu => self.cpu += 1,
            ErrKind::Memory => self.memory += 1,
            ErrKind::Pcie => self.pcie += 1,
            ErrKind::Gpu => self.gpu += 1,
            ErrKind::Storage => self.storage += 1,
            ErrKind::Thermal => self.thermal += 1,
            ErrKind::Oom => self.oom += 1,
        }
    }

    /// Resumo em uma linha, ou `None` se está tudo limpo.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = [
            (ErrKind::Cpu, self.cpu),
            (ErrKind::Memory, self.memory),
            (ErrKind::Pcie, self.pcie),
            (ErrKind::Gpu, self.gpu),
            (ErrKind::Storage, self.storage),
            (ErrKind::Thermal, self.thermal),
            (ErrKind::Oom, self.oom),
        ]
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| format!("{}: {n}", k.title()))
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Separa "data host kernel: mensagem" (journalctl) ou "data mensagem" (dmesg) em (instante, mensagem).
pub fn split_log_line(line: &str) -> (Option<i64>, &str) {
    let first_end = line.find(' ').unwrap_or(line.len());
    let ts = parse_iso(&line[..first_end]);
    let rest = line[first_end..].trim_start();
    let msg = rest.find("kernel: ").map_or(rest, |i| &rest[i + 8..]);
    (ts, msg)
}

/// Conta os problemas em um texto de log, considerando só linhas a partir de `since_unix` (as sem data entram).
pub fn scan_text(text: &str, since_unix: Option<i64>) -> HardwareErrors {
    let mut e = HardwareErrors::default();
    for line in text.lines() {
        let (ts, msg) = split_log_line(line);
        if let (Some(since), Some(t)) = (since_unix, ts)
            && t < since {
                continue;
            }
        if let Some(k) = classify_line(msg) {
            e.add(k);
            e.last_detail = Some(format!("{} · {}", k.title(), trunc(msg.trim(), 140)));
        }
    }
    e
}

/// Lê o log do kernel desde `since_unix` (ou do boot atual, se `None`).
pub fn errors_since(since_unix: Option<u64>) -> HardwareErrors {
    let since = since_unix.map(|s| s as i64);
    let attempts: [(&str, &[&str]); 2] = [("journalctl", &["-k", "-b", "-o", "short-iso", "--no-pager", "-q"]), ("dmesg", &["--time-format", "iso"])];
    let mut last_err = String::from("não encontrei journalctl nem dmesg");
    for (cmd, args) in attempts {
        let Some(out) = run_maybe_priv(cmd, args, Duration::from_secs(15)) else { continue };
        if out.ok() && !out.stdout.trim().is_empty() {
            return scan_text(&out.stdout, since);
        }
        let text = out.combined();
        last_err = if text.to_lowercase().contains("permission") || text.to_lowercase().contains("not permitted") {
            format!("sem permissão para ler o log do kernel ({cmd}): rode com sudo ou entre no grupo adm/systemd-journal")
        } else if text.trim().is_empty() {
            format!("{cmd} não devolveu nenhuma linha")
        } else {
            format!("{cmd}: {}", out.last_line())
        };
    }
    HardwareErrors { error: Some(last_err), ..Default::default() }
}

/// Contadores de erro de RAM do EDAC (corrigidos, não corrigidos). `None` sem controlador EDAC.
pub fn edac_counts(r: &Roots) -> Option<(u64, u64)> {
    let base = r.sys.join("devices/system/edac/mc");
    let mcs: Vec<String> = dir_names(&base).into_iter().filter(|n| n.starts_with("mc") && n[2..].chars().all(|c| c.is_ascii_digit())).collect();
    if mcs.is_empty() {
        return None;
    }
    let (mut ce, mut ue) = (0, 0);
    for mc in mcs {
        ce += read_num::<u64>(base.join(&mc).join("ce_count")).unwrap_or(0);
        ue += read_num::<u64>(base.join(&mc).join("ue_count")).unwrap_or(0);
    }
    Some((ce, ue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_kernel_messages() {
        let cases = [
            ("mce: [Hardware Error]: Machine check events logged", ErrKind::Cpu),
            ("[Hardware Error]: CPU:0 (19:61:2) MC1_STATUS[Over|CE|MiscV]", ErrKind::Cpu),
            ("EDAC MC0: 1 CE memory read error on CPU_SrcID#0_Ha#0_Chan#1_DIMM#0", ErrKind::Memory),
            ("Memory failure: 0x1234: recovery action for dirty LRU page: Recovered", ErrKind::Memory),
            ("pcieport 0000:00:01.1: AER: Corrected error received: 0000:01:00.0", ErrKind::Pcie),
            ("pcieport 0000:00:01.1: PCIe Bus Error: severity=Uncorrected (Non-Fatal)", ErrKind::Pcie),
            ("NVRM: Xid (PCI:0000:01:00): 79, pid=1234, GPU has fallen off the bus.", ErrKind::Gpu),
            ("amdgpu 0000:03:00.0: amdgpu: ring gfx_0.0.0 timeout, signaled seq=1, emitted seq=3", ErrKind::Gpu),
            ("i915 0000:00:02.0: [drm] GPU HANG: ecode 12:1:85dffffb", ErrKind::Gpu),
            ("amdgpu 0000:03:00.0: amdgpu: GPU reset begin!", ErrKind::Gpu),
            ("blk_update_request: I/O error, dev sda, sector 123 op 0x0:(READ)", ErrKind::Storage),
            ("nvme nvme0: controller is down; will reset: CSTS=0x3", ErrKind::Storage),
            ("EXT4-fs error (device sda1): ext4_find_entry:1455: inode #2", ErrKind::Storage),
            ("CPU0: Core temperature above threshold, cpu clock throttled (total events = 1)", ErrKind::Thermal),
            ("Out of memory: Killed process 4242 (chrome) total-vm:1234kB", ErrKind::Oom),
        ];
        for (msg, want) in cases {
            assert_eq!(classify_line(msg), Some(want), "{msg}");
        }
        for ok in ["usb 1-2: new high-speed USB device number 5 using xhci_hcd", "EXT4-fs (sda1): mounted filesystem with ordered data mode", "nvme nvme0: 8/0/0 default/read/poll queues", "amdgpu 0000:03:00.0: amdgpu: SMU is initialized successfully!", "mce: CPU0: Package temperature/speed normal"] {
            assert_eq!(classify_line(ok), None, "{ok}");
        }
    }

    #[test]
    fn parses_iso_timestamps_with_offsets() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-10-22T23:13:01-0300"), Some(1_792_721_581));
        assert_eq!(parse_iso("2026-10-23T02:13:01+0000"), Some(1_792_721_581));
        assert_eq!(parse_iso("2026-10-22T23:13:01,123456-03:00"), Some(1_792_721_581));
        assert_eq!(parse_iso("2026-10-22 23:13:01.5-03"), Some(1_792_721_581));
        assert_eq!(parse_iso("2024-02-29T12:00:00Z"), Some(1_709_208_000), "ano bissexto");
        assert_eq!(parse_iso("não é data"), None);
        assert_eq!(parse_iso("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn splits_journal_and_dmesg_lines() {
        let (t, m) = split_log_line("2026-10-22T23:13:01-0300 pc kernel: NVRM: Xid 79");
        assert_eq!((t, m), (Some(1_792_721_581), "NVRM: Xid 79"));
        let (t, m) = split_log_line("2026-10-22T23:13:01,000000-03:00 EDAC MC0: 1 CE");
        assert_eq!((t, m), (Some(1_792_721_581), "EDAC MC0: 1 CE"));
        let (t, m) = split_log_line("linha sem data kernel: texto");
        assert_eq!((t, m), (None, "texto"));
    }

    #[test]
    fn scan_counts_only_lines_after_the_cutoff() {
        let log = "\
2026-10-22T20:00:00-0300 pc kernel: NVRM: Xid (PCI:0000:01:00): 79, GPU has fallen off the bus.
2026-10-22T23:00:00-0300 pc kernel: mce: [Hardware Error]: Machine check events logged
2026-10-22T23:05:00-0300 pc kernel: pcieport 0000:00:01.1: AER: Corrected error received: 0000:01:00.0
2026-10-22T23:06:00-0300 pc kernel: Out of memory: Killed process 1 (x)
2026-10-22T23:07:00-0300 pc kernel: usb 1-1: reset high-speed USB device
";
        let all = scan_text(log, None);
        assert_eq!((all.gpu, all.cpu, all.pcie, all.oom), (1, 1, 1, 1));
        assert_eq!(all.total(), 3, "OOM não é falha de hardware");
        let since = parse_iso("2026-10-22T22:59:00-0300");
        let e = scan_text(log, since);
        assert_eq!((e.gpu, e.cpu, e.pcie, e.oom), (0, 1, 1, 1));
        assert!(e.last_detail.as_deref().unwrap().starts_with("Falta de memória"));
        assert_eq!(e.summary().unwrap(), "CPU (MCE): 1 · PCIe: 1 · Falta de memória: 1");
        assert!(scan_text("", None).summary().is_none());
    }

    #[test]
    fn reads_edac_counters() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("sys/devices/system/edac/mc");
        for (mc, ce, ue) in [("mc0", "3", "0"), ("mc1", "2", "1")] {
            std::fs::create_dir_all(base.join(mc)).unwrap();
            std::fs::write(base.join(mc).join("ce_count"), format!("{ce}\n")).unwrap();
            std::fs::write(base.join(mc).join("ue_count"), format!("{ue}\n")).unwrap();
        }
        assert_eq!(edac_counts(&Roots::under(d.path())), Some((5, 1)));
        assert_eq!(edac_counts(&Roots::under(tempfile::tempdir().unwrap().path())), None);
    }
}
