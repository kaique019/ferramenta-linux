//! Energia: bateria, fonte de alimentação, governador da CPU, perfil de plataforma e boost.
//!
//! Tudo é lido de /sys (sem privilégios). Alterações passam por `write_priv` e só aceitam valores que o próprio
//! kernel lista como disponíveis, então nunca gravam texto arbitrário em arquivos do sistema.

use std::path::Path;

use serde::Serialize;

use crate::util::{Roots, dir_names, natural_cmp, read_num, read_trim, write_priv};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Battery {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub technology: String,
    /// "Charging", "Discharging", "Full", "Not charging"...
    pub status: String,
    /// Carga atual em %.
    pub capacity: Option<f64>,
    /// Capacidade total atual em Wh.
    pub full_wh: Option<f64>,
    /// Capacidade de projeto em Wh.
    pub design_wh: Option<f64>,
    /// Saúde: capacidade atual / de projeto, em %.
    pub health: Option<f64>,
    pub cycles: Option<u64>,
    pub voltage_v: Option<f64>,
    /// Consumo (descarregando) ou potência de carga, em W.
    pub power_w: Option<f64>,
    /// Tempo estimado até esvaziar/encher, em minutos.
    pub minutes_left: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuPower {
    pub driver: Option<String>,
    pub governor: Option<String>,
    pub governors: Vec<String>,
    pub epp: Option<String>,
    pub epp_options: Vec<String>,
    pub min_mhz: Option<f64>,
    pub max_mhz: Option<f64>,
    /// `true` se o turbo/boost está ligado.
    pub boost: Option<bool>,
    /// Perfil de plataforma (power-saver, balanced, performance).
    pub platform_profile: Option<String>,
    pub platform_profiles: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PowerInfo {
    pub on_ac: Option<bool>,
    pub batteries: Vec<Battery>,
    pub cpu: CpuPower,
}

fn micro(path: &Path) -> Option<f64> {
    read_num::<f64>(path).map(|v| v / 1e6)
}

fn words(path: &Path) -> Vec<String> {
    read_trim(path).map(|s| s.split_whitespace().map(str::to_string).collect()).unwrap_or_default()
}

fn read_battery(dir: &Path, name: &str) -> Battery {
    let s = |f: &str| read_trim(dir.join(f)).unwrap_or_default();
    let voltage_v = micro(&dir.join("voltage_now")).filter(|v| *v > 0.0);
    // Algumas baterias informam energia (µWh), outras carga (µAh); nesse caso converte com a tensão.
    let to_wh = |energy: &str, charge: &str| -> Option<f64> {
        micro(&dir.join(energy)).filter(|v| *v > 0.0).or_else(|| micro(&dir.join(charge)).filter(|v| *v > 0.0).zip(voltage_v.or_else(|| micro(&dir.join("voltage_min_design")))).map(|(ah, v)| ah * v))
    };
    let full_wh = to_wh("energy_full", "charge_full");
    let design_wh = to_wh("energy_full_design", "charge_full_design");
    let now_wh = to_wh("energy_now", "charge_now");
    let health = full_wh.zip(design_wh).filter(|(_, d)| *d > 0.0).map(|(f, d)| (f / d * 100.0).min(100.0));
    let power_w = micro(&dir.join("power_now")).filter(|v| *v > 0.0).or_else(|| micro(&dir.join("current_now")).filter(|v| *v > 0.0).zip(voltage_v).map(|(a, v)| a * v));
    let status = s("status");
    let minutes_left = match (power_w, now_wh, full_wh) {
        (Some(p), Some(now), _) if p > 0.5 && status == "Discharging" => Some((now / p * 60.0) as u64),
        (Some(p), Some(now), Some(full)) if p > 0.5 && status == "Charging" => Some(((full - now).max(0.0) / p * 60.0) as u64),
        _ => None,
    };
    Battery {
        name: name.to_string(),
        manufacturer: s("manufacturer"),
        model: s("model_name"),
        technology: s("technology"),
        status,
        capacity: read_num::<f64>(dir.join("capacity")),
        full_wh,
        design_wh,
        health,
        cycles: read_num::<u64>(dir.join("cycle_count")).filter(|c| *c > 0),
        voltage_v,
        power_w,
        minutes_left,
    }
}

pub fn collect_cpu(r: &Roots) -> CpuPower {
    let cf = r.sys.join("devices/system/cpu/cpu0/cpufreq");
    let pstate = r.sys.join("devices/system/cpu/intel_pstate/no_turbo");
    let boost_file = r.sys.join("devices/system/cpu/cpufreq/boost");
    let boost = if let Some(v) = read_num::<u8>(&pstate) {
        Some(v == 0)
    } else {
        read_num::<u8>(&boost_file).map(|v| v == 1)
    };
    let pp = r.sys.join("firmware/acpi/platform_profile");
    CpuPower {
        driver: read_trim(cf.join("scaling_driver")),
        governor: read_trim(cf.join("scaling_governor")),
        governors: words(&cf.join("scaling_available_governors")),
        epp: read_trim(cf.join("energy_performance_preference")),
        epp_options: words(&cf.join("energy_performance_available_preferences")),
        min_mhz: read_num::<f64>(cf.join("cpuinfo_min_freq")).map(|k| k / 1000.0),
        max_mhz: read_num::<f64>(cf.join("cpuinfo_max_freq")).map(|k| k / 1000.0),
        boost,
        platform_profile: read_trim(&pp),
        platform_profiles: words(&r.sys.join("firmware/acpi/platform_profile_choices")),
    }
}

pub fn collect(r: &Roots) -> PowerInfo {
    let ps = r.sys.join("class/power_supply");
    let mut names = dir_names(&ps);
    names.sort_by(|a, b| natural_cmp(a, b));
    let mut info = PowerInfo { cpu: collect_cpu(r), ..Default::default() };
    for n in names {
        let d = ps.join(&n);
        match read_trim(d.join("type")).as_deref() {
            Some("Battery") if read_trim(d.join("scope")).as_deref() != Some("Device") => info.batteries.push(read_battery(&d, &n)),
            Some("Mains") | Some("USB") if n.starts_with("AC") || n.starts_with("ADP") || read_trim(d.join("type")).as_deref() == Some("Mains") => {
                let on = read_num::<u8>(d.join("online")).map(|v| v == 1);
                info.on_ac = match (info.on_ac, on) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (a, b) => b.or(a),
                };
            }
            _ => {}
        }
    }
    info
}

/// Governadores que o kernel aceita na CPU 0.
fn validate(value: &str, allowed: &[String], what: &str) -> Result<(), String> {
    if allowed.is_empty() {
        return Err(format!("este sistema não expõe opções de {what}"));
    }
    if allowed.iter().any(|a| a == value) { Ok(()) } else { Err(format!("{what} \"{value}\" inválido; opções: {}", allowed.join(", "))) }
}

/// Governador de cada CPU (nome da pasta `cpuN` -> governador), para restaurar exatamente o que havia.
pub fn governors(r: &Roots) -> std::collections::BTreeMap<String, String> {
    let base = r.sys.join("devices/system/cpu");
    dir_names(&base)
        .into_iter()
        .filter(|n| n.strip_prefix("cpu").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())))
        .filter_map(|n| read_trim(base.join(&n).join("cpufreq/scaling_governor")).map(|g| (n, g)))
        .collect()
}

/// Devolve a cada CPU o governador de `snapshot` (os que o kernel não aceita mais são ignorados). Quantas voltaram.
pub fn restore_governors(r: &Roots, snapshot: &std::collections::BTreeMap<String, String>) -> usize {
    let base = r.sys.join("devices/system/cpu");
    let allowed = collect_cpu(r).governors;
    snapshot.iter().filter(|(cpu, g)| allowed.contains(g) && write_priv(&base.join(cpu).join("cpufreq/scaling_governor"), g).is_ok()).count()
}

/// Define o governador em todas as CPUs. Devolve quantas foram alteradas.
pub fn set_governor(r: &Roots, governor: &str) -> Result<usize, String> {
    let cpu = collect_cpu(r);
    validate(governor, &cpu.governors, "governador")?;
    set_per_cpu(r, "scaling_governor", governor)
}

/// Define a preferência energia/desempenho (EPP) em todas as CPUs.
pub fn set_epp(r: &Roots, epp: &str) -> Result<usize, String> {
    let cpu = collect_cpu(r);
    validate(epp, &cpu.epp_options, "EPP")?;
    set_per_cpu(r, "energy_performance_preference", epp)
}

fn set_per_cpu(r: &Roots, file: &str, value: &str) -> Result<usize, String> {
    let base = r.sys.join("devices/system/cpu");
    let mut n = 0;
    let mut first_err = None;
    for name in dir_names(&base) {
        let is_cpu = name.strip_prefix("cpu").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
        let f = base.join(&name).join("cpufreq").join(file);
        if is_cpu && f.exists() {
            match write_priv(&f, value) {
                Ok(()) => n += 1,
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
    }
    match (n, first_err) {
        (0, Some(e)) => Err(e),
        (0, None) => Err("nenhuma CPU com cpufreq".into()),
        _ => Ok(n),
    }
}

/// Define o perfil de plataforma ACPI (power-saver/balanced/performance).
pub fn set_platform_profile(r: &Roots, profile: &str) -> Result<(), String> {
    let cpu = collect_cpu(r);
    validate(profile, &cpu.platform_profiles, "perfil")?;
    write_priv(&r.sys.join("firmware/acpi/platform_profile"), profile)
}

/// Liga ou desliga o turbo/boost.
pub fn set_boost(r: &Roots, on: bool) -> Result<(), String> {
    let pstate = r.sys.join("devices/system/cpu/intel_pstate/no_turbo");
    if pstate.exists() {
        return write_priv(&pstate, if on { "0" } else { "1" });
    }
    let b = r.sys.join("devices/system/cpu/cpufreq/boost");
    if b.exists() {
        return write_priv(&b, if on { "1" } else { "0" });
    }
    Err("este sistema não expõe controle de boost".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn fake(root: &Path) {
        let b = "sys/class/power_supply/BAT0/";
        for (k, v) in [("type", "Battery"), ("status", "Discharging"), ("capacity", "50"), ("energy_full", "40000000"), ("energy_full_design", "50000000"), ("energy_now", "20000000"), ("power_now", "10000000"), ("voltage_now", "12000000"), ("cycle_count", "312"), ("model_name", "DELL XYZ"), ("manufacturer", "SMP"), ("technology", "Li-ion")] {
            write(root, &format!("{b}{k}"), &format!("{v}\n"));
        }
        write(root, "sys/class/power_supply/AC/type", "Mains\n");
        write(root, "sys/class/power_supply/AC/online", "0\n");
        write(root, "sys/class/power_supply/hidpp_battery_0/type", "Battery\n");
        write(root, "sys/class/power_supply/hidpp_battery_0/scope", "Device\n");
        for c in 0..2 {
            let cf = format!("sys/devices/system/cpu/cpu{c}/cpufreq/");
            write(root, &format!("{cf}scaling_governor"), "powersave\n");
            write(root, &format!("{cf}scaling_available_governors"), "performance powersave\n");
            write(root, &format!("{cf}scaling_driver"), "intel_pstate\n");
            write(root, &format!("{cf}energy_performance_preference"), "balance_performance\n");
            write(root, &format!("{cf}energy_performance_available_preferences"), "default performance balance_performance balance_power power\n");
            write(root, &format!("{cf}cpuinfo_min_freq"), "400000\n");
            write(root, &format!("{cf}cpuinfo_max_freq"), "4800000\n");
        }
        write(root, "sys/devices/system/cpu/cpufreq/policy0/x", "");
        write(root, "sys/devices/system/cpu/intel_pstate/no_turbo", "0\n");
        write(root, "sys/firmware/acpi/platform_profile", "balanced\n");
        write(root, "sys/firmware/acpi/platform_profile_choices", "low-power balanced performance\n");
    }

    #[test]
    fn reads_battery_health_and_time_left() {
        let d = tempfile::tempdir().unwrap();
        fake(d.path());
        let p = collect(&Roots::under(d.path()));
        assert_eq!(p.on_ac, Some(false));
        assert_eq!(p.batteries.len(), 1, "bateria de periférico (scope=Device) não conta");
        let b = &p.batteries[0];
        assert_eq!(b.capacity, Some(50.0));
        assert_eq!(b.health.map(|h| h.round()), Some(80.0));
        assert_eq!(b.power_w, Some(10.0));
        assert_eq!(b.minutes_left, Some(120));
        assert_eq!(b.cycles, Some(312));
    }

    #[test]
    fn converts_charge_based_batteries_using_voltage() {
        let d = tempfile::tempdir().unwrap();
        let b = "sys/class/power_supply/BAT1/";
        write(d.path(), &format!("{b}type"), "Battery\n");
        write(d.path(), &format!("{b}charge_full"), "4000000\n");
        write(d.path(), &format!("{b}charge_full_design"), "5000000\n");
        write(d.path(), &format!("{b}voltage_now"), "10000000\n");
        let bat = &collect(&Roots::under(d.path())).batteries[0];
        assert_eq!(bat.full_wh, Some(40.0));
        assert_eq!(bat.design_wh, Some(50.0));
    }

    #[test]
    fn health_never_exceeds_100() {
        let d = tempfile::tempdir().unwrap();
        let b = "sys/class/power_supply/BAT0/";
        write(d.path(), &format!("{b}type"), "Battery\n");
        write(d.path(), &format!("{b}energy_full"), "52000000\n");
        write(d.path(), &format!("{b}energy_full_design"), "50000000\n");
        assert_eq!(collect(&Roots::under(d.path())).batteries[0].health, Some(100.0));
    }

    #[test]
    fn desktop_without_battery_is_fine() {
        let d = tempfile::tempdir().unwrap();
        let p = collect(&Roots::under(d.path()));
        assert!(p.batteries.is_empty() && p.on_ac.is_none() && p.cpu.governor.is_none());
    }

    #[test]
    fn reads_cpu_power_settings() {
        let d = tempfile::tempdir().unwrap();
        fake(d.path());
        let c = collect_cpu(&Roots::under(d.path()));
        assert_eq!(c.governor.as_deref(), Some("powersave"));
        assert_eq!(c.governors, ["performance", "powersave"]);
        assert_eq!(c.boost, Some(true));
        assert_eq!(c.max_mhz, Some(4800.0));
        assert_eq!(c.platform_profile.as_deref(), Some("balanced"));
    }

    #[test]
    fn sets_governor_on_every_cpu_and_rejects_unknown_values() {
        let d = tempfile::tempdir().unwrap();
        fake(d.path());
        let r = Roots::under(d.path());
        assert_eq!(set_governor(&r, "performance"), Ok(2));
        for c in 0..2 {
            assert_eq!(read_trim(d.path().join(format!("sys/devices/system/cpu/cpu{c}/cpufreq/scaling_governor"))).as_deref(), Some("performance"));
        }
        assert!(set_governor(&r, "../../etc/passwd").is_err());
        assert!(set_governor(&r, "turbo").is_err());
        assert_eq!(set_epp(&r, "power"), Ok(2));
        assert!(set_platform_profile(&r, "balanced").is_ok());
        assert!(set_platform_profile(&r, "hack").is_err());
    }

    #[test]
    fn governors_snapshot_and_restore_are_per_cpu() {
        let d = tempfile::tempdir().unwrap();
        fake(d.path());
        let gov = |c: u32| d.path().join(format!("sys/devices/system/cpu/cpu{c}/cpufreq/scaling_governor"));
        fs::write(gov(1), "performance\n").unwrap(); // CPUs com governadores diferentes
        let r = Roots::under(d.path());
        let snap = governors(&r);
        assert_eq!(snap.get("cpu0").map(String::as_str), Some("powersave"));
        assert_eq!(snap.get("cpu1").map(String::as_str), Some("performance"));
        set_governor(&r, "performance").unwrap();
        assert_eq!(restore_governors(&r, &snap), 2);
        assert_eq!(read_trim(gov(0)).as_deref(), Some("powersave"));
        assert_eq!(read_trim(gov(1)).as_deref(), Some("performance"), "cada CPU volta ao seu próprio valor");
        let mut bad = snap.clone();
        bad.insert("cpu0".into(), "../../x".into());
        assert_eq!(restore_governors(&r, &bad), 1, "valor que o kernel não lista é ignorado");
    }

    #[test]
    fn boost_toggle_uses_no_turbo_inverted() {
        let d = tempfile::tempdir().unwrap();
        fake(d.path());
        let r = Roots::under(d.path());
        set_boost(&r, false).unwrap();
        assert_eq!(collect_cpu(&r).boost, Some(false));
        set_boost(&r, true).unwrap();
        assert_eq!(collect_cpu(&r).boost, Some(true));
        let empty = tempfile::tempdir().unwrap();
        assert!(set_boost(&Roots::under(empty.path()), true).is_err());
    }
}
