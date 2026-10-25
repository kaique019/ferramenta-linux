//! Verificador de estabilidade (para quem fez overclock ou undervolt): estressa CPU e RAM e confere se o kernel
//! registrou erros de hardware durante o teste. O veredito é uma função pura, fácil de testar.

use crate::hwerrors::{self, HardwareErrors};
use crate::settings::OcRecord;
use crate::stress::{self, Probe, StopReason, StressConfig, StressReport};
use crate::util::{Cancel, Roots, now_unix};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum OcVerdict {
    Stable,
    StableWithWarnings,
    Unstable,
    Incomplete,
}

impl OcVerdict {
    pub fn label(self) -> &'static str {
        match self {
            OcVerdict::Stable => "Estável",
            OcVerdict::StableWithWarnings => "Estável, com avisos",
            OcVerdict::Unstable => "INSTÁVEL",
            OcVerdict::Incomplete => "Incompleto",
        }
    }
}

/// `edac_delta` = (erros corrigidos, não corrigidos) ocorridos durante o teste.
pub fn judge(rep: &StressReport, errs: &HardwareErrors, edac_delta: (u64, u64)) -> (OcVerdict, String) {
    let mut problems = vec![];
    if rep.mem_errors > 0 {
        problems.push(format!("{} erro(s) de memória no teste", rep.mem_errors));
    }
    if errs.total() > 0 {
        problems.push(format!("erros de hardware no log do kernel ({})", errs.summary().unwrap_or_default()));
    }
    if edac_delta.1 > 0 {
        problems.push(format!("{} erro(s) de RAM não corrigidos (EDAC)", edac_delta.1));
    }
    if !problems.is_empty() {
        return (OcVerdict::Unstable, problems.join("; "));
    }
    if !matches!(rep.reason, StopReason::Time) {
        return (OcVerdict::Incomplete, rep.reason.text());
    }
    let mut warns = vec![];
    if errs.thermal > 0 {
        warns.push(format!("{} evento(s) de limitação térmica", errs.thermal));
    }
    if edac_delta.0 > 0 {
        warns.push(format!("{} erro(s) de RAM corrigidos (EDAC)", edac_delta.0));
    }
    if let Some(e) = &errs.error {
        warns.push(format!("log do kernel não verificado: {e}"));
    }
    if warns.is_empty() { (OcVerdict::Stable, "Nenhum erro durante o teste".into()) } else { (OcVerdict::StableWithWarnings, warns.join("; ")) }
}

pub struct OcOutcome {
    pub verdict: OcVerdict,
    pub summary: String,
    pub report: StressReport,
    pub errors: HardwareErrors,
}

impl OcOutcome {
    pub fn record(&self) -> OcRecord {
        OcRecord { date: now_unix(), target: "cpu+ram".into(), minutes: self.report.seconds / 60.0, verdict: self.verdict.label().into(), summary: self.summary.clone(), max_cpu_temp: self.report.max_temp, max_gpu_temp: None }
    }
}

/// Núcleo testável: as leituras do log e do EDAC entram como funções.
pub fn run_with(cfg: &StressConfig, probe: &mut dyn Probe, cancel: &Cancel, progress: &dyn Fn(&stress::StressSample), log_errors: &dyn Fn(u64) -> HardwareErrors, edac: &dyn Fn() -> Option<(u64, u64)>) -> OcOutcome {
    let start = now_unix();
    let edac_before = edac();
    let report = stress::run(cfg, probe, cancel, progress);
    let errors = log_errors(start);
    let delta = match (edac_before, edac()) {
        (Some(a), Some(b)) => (b.0.saturating_sub(a.0), b.1.saturating_sub(a.1)),
        _ => (0, 0),
    };
    let (verdict, summary) = judge(&report, &errors, delta);
    OcOutcome { verdict, summary, report, errors }
}

pub fn run(cfg: &StressConfig, cancel: &Cancel, progress: &dyn Fn(&stress::StressSample)) -> OcOutcome {
    let mut probe = stress::SystemProbe::new();
    let roots = Roots::system();
    run_with(cfg, &mut probe, cancel, progress, &|s| hwerrors::errors_since(Some(s)), &|| hwerrors::edac_counts(&roots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stress::ProbeReading;
    use std::time::Duration;

    fn rep(reason: StopReason, mem_errors: u64) -> StressReport {
        StressReport { reason, seconds: 60.0, samples: vec![], max_temp: Some(80.0), avg_load: None, avg_power: None, mem_bytes: 0, mem_passes: 1, mem_errors, log: vec![] }
    }

    #[test]
    fn clean_run_is_stable() {
        assert_eq!(judge(&rep(StopReason::Time, 0), &HardwareErrors::default(), (0, 0)).0, OcVerdict::Stable);
    }

    #[test]
    fn any_error_signal_makes_it_unstable() {
        assert_eq!(judge(&rep(StopReason::MemoryErrors(3), 3), &HardwareErrors::default(), (0, 0)).0, OcVerdict::Unstable);
        let e = HardwareErrors { cpu: 1, ..Default::default() };
        assert_eq!(judge(&rep(StopReason::Time, 0), &e, (0, 0)).0, OcVerdict::Unstable);
        assert_eq!(judge(&rep(StopReason::Time, 0), &HardwareErrors::default(), (0, 1)).0, OcVerdict::Unstable);
    }

    #[test]
    fn thermal_stop_or_cancel_is_incomplete_not_stable() {
        assert_eq!(judge(&rep(StopReason::Temperature(95.0), 0), &HardwareErrors::default(), (0, 0)).0, OcVerdict::Incomplete);
        assert_eq!(judge(&rep(StopReason::Cancelled, 0), &HardwareErrors::default(), (0, 0)).0, OcVerdict::Incomplete);
    }

    #[test]
    fn warnings_do_not_fail_but_are_reported() {
        let e = HardwareErrors { thermal: 2, ..Default::default() };
        let (v, s) = judge(&rep(StopReason::Time, 0), &e, (4, 0));
        assert_eq!(v, OcVerdict::StableWithWarnings);
        assert!(s.contains("térmica") && s.contains("corrigidos"));
        let unread = HardwareErrors { error: Some("sem permissão".into()), ..Default::default() };
        assert_eq!(judge(&rep(StopReason::Time, 0), &unread, (0, 0)).0, OcVerdict::StableWithWarnings, "log ilegível nunca vira 'Estável' silencioso");
    }

    struct Fake;
    impl Probe for Fake {
        fn read(&mut self) -> ProbeReading {
            ProbeReading { cpu_temp: Some(60.0), cpu_load: Some(100.0), power_w: None, clock_mhz: None }
        }
    }

    #[test]
    fn run_with_uses_edac_delta_and_log_errors() {
        use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
        let n = AtomicU64::new(0);
        let cfg = StressConfig { duration: Duration::from_millis(1200), sample_interval: Duration::from_millis(200), ..Default::default() };
        // o contador EDAC "sobe" entre a primeira e a segunda leitura: 0 -> 1 não corrigido
        let o = run_with(&cfg, &mut Fake, &Cancel::new(), &|_| {}, &|_| HardwareErrors::default(), &|| Some((0, n.fetch_add(1, SeqCst))));
        assert_eq!(o.verdict, OcVerdict::Unstable);
        let o = run_with(&cfg, &mut Fake, &Cancel::new(), &|_| {}, &|_| HardwareErrors::default(), &|| Some((0, 0)));
        assert_eq!(o.verdict, OcVerdict::Stable);
        assert_eq!(o.record().verdict, "Estável");
    }
}
