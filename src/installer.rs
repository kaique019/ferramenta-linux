//! Instalador de utilitários (MangoHud, GameMode, lm-sensors, smartmontools...) pelo gerenciador de pacotes da distro.
//!
//! O usuário só escolhe um item do catálogo fixo; o comando é montado como lista de argumentos (sem shell), então
//! nada que venha da entrada vira parte de um comando.

use std::time::Duration;

use serde::Serialize;

use crate::util::{Roots, parse_env_file, read_trim, run_priv, which};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Pm {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl Pm {
    pub fn name(self) -> &'static str {
        match self {
            Pm::Apt => "apt",
            Pm::Dnf => "dnf",
            Pm::Pacman => "pacman",
            Pm::Zypper => "zypper",
        }
    }
}

struct AppDef {
    id: &'static str,
    title: &'static str,
    /// Executável usado para saber se já está instalado.
    binary: &'static str,
    /// Nome do pacote em (apt, dnf, pacman, zypper).
    pkgs: [&'static str; 4],
}

const CATALOG: &[AppDef] = &[
    AppDef { id: "mangohud", title: "MangoHud (overlay de FPS)", binary: "mangohud", pkgs: ["mangohud", "mangohud", "mangohud", "mangohud"] },
    AppDef { id: "gamemode", title: "GameMode (otimizações durante o jogo)", binary: "gamemoderun", pkgs: ["gamemode", "gamemode", "gamemode", "gamemode"] },
    AppDef { id: "lm-sensors", title: "lm-sensors (temperaturas e ventoinhas)", binary: "sensors", pkgs: ["lm-sensors", "lm_sensors", "lm_sensors", "sensors"] },
    AppDef { id: "smartmontools", title: "smartmontools (saúde dos discos)", binary: "smartctl", pkgs: ["smartmontools", "smartmontools", "smartmontools", "smartmontools"] },
    AppDef { id: "pciutils", title: "pciutils (lspci)", binary: "lspci", pkgs: ["pciutils", "pciutils", "pciutils", "pciutils"] },
    AppDef { id: "dmidecode", title: "dmidecode (módulos de memória)", binary: "dmidecode", pkgs: ["dmidecode", "dmidecode", "dmidecode", "dmidecode"] },
    AppDef { id: "nvtop", title: "nvtop (monitor de GPU)", binary: "nvtop", pkgs: ["nvtop", "nvtop", "nvtop", "nvtop"] },
    AppDef { id: "btop", title: "btop (monitor do sistema)", binary: "btop", pkgs: ["btop", "btop", "btop", "btop"] },
];

#[derive(Debug, Clone, Serialize)]
pub struct AppStatus {
    pub id: &'static str,
    pub title: &'static str,
    pub installed: bool,
}

/// Gerenciador de pacotes da distro: pelo os-release (ID/ID_LIKE) e, na dúvida, pelo que existe no PATH.
pub fn detect(r: &Roots) -> Option<Pm> {
    let kv = parse_env_file(&read_trim(r.etc.join("os-release")).or_else(|| read_trim("/usr/lib/os-release")).unwrap_or_default());
    let ids: Vec<String> = kv.iter().filter(|(k, _)| k == "ID" || k == "ID_LIKE").flat_map(|(_, v)| v.split_whitespace().map(|s| s.trim_matches('"').to_lowercase()).collect::<Vec<_>>()).collect();
    let by_id = ids.iter().find_map(|i| match i.as_str() {
        "debian" | "ubuntu" | "linuxmint" | "pop" => Some(Pm::Apt),
        "fedora" | "rhel" | "centos" | "nobara" => Some(Pm::Dnf),
        "arch" | "manjaro" | "endeavouros" | "cachyos" => Some(Pm::Pacman),
        "opensuse" | "suse" | "sles" => Some(Pm::Zypper),
        _ => None,
    });
    by_id.or_else(|| [(Pm::Apt, "apt-get"), (Pm::Dnf, "dnf"), (Pm::Pacman, "pacman"), (Pm::Zypper, "zypper")].into_iter().find(|(_, c)| which(c).is_some()).map(|(p, _)| p))
}

pub fn catalog() -> Vec<AppStatus> {
    CATALOG.iter().map(|a| AppStatus { id: a.id, title: a.title, installed: which(a.binary).is_some() }).collect()
}

fn find(id: &str) -> Result<&'static AppDef, String> {
    CATALOG.iter().find(|a| a.id == id).ok_or_else(|| format!("programa desconhecido: {id} (opções: {})", CATALOG.iter().map(|a| a.id).collect::<Vec<_>>().join(", ")))
}

/// Comando completo (programa + argumentos) para instalar `id`.
pub fn plan(pm: Pm, id: &str) -> Result<Vec<String>, String> {
    let app = find(id)?;
    let pkg = app.pkgs[pm as usize];
    let base: &[&str] = match pm {
        Pm::Apt => &["apt-get", "install", "-y"],
        Pm::Dnf => &["dnf", "install", "-y"],
        Pm::Pacman => &["pacman", "-S", "--noconfirm", "--needed"],
        Pm::Zypper => &["zypper", "--non-interactive", "install"],
    };
    Ok(base.iter().chain(std::iter::once(&pkg)).map(|s| s.to_string()).collect())
}

/// Instala com privilégios (root ou sudo já autorizado). Devolve a última linha da saída em caso de erro.
pub fn install(r: &Roots, id: &str) -> Result<(), String> {
    let pm = detect(r).ok_or("não reconheci o gerenciador de pacotes desta distro")?;
    let cmd = plan(pm, id)?;
    let args: Vec<&str> = cmd[1..].iter().map(String::as_str).collect();
    let out = run_priv(&cmd[0], &args, Duration::from_secs(900)).ok_or("precisa de privilégios: rode com sudo (ou autorize antes com `sudo -v`)")?;
    if out.ok() { Ok(()) } else { Err(out.last_line()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn os_release(text: &str) -> (tempfile::TempDir, Roots) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("etc")).unwrap();
        fs::write(d.path().join("etc/os-release"), text).unwrap();
        let r = Roots::under(d.path());
        (d, r)
    }

    #[test]
    fn detects_family_from_id_and_id_like() {
        assert_eq!(detect(&os_release("ID=ubuntu\nID_LIKE=debian\n").1), Some(Pm::Apt));
        assert_eq!(detect(&os_release("ID=nobara\nID_LIKE=\"rhel centos fedora\"\n").1), Some(Pm::Dnf));
        assert_eq!(detect(&os_release("ID=cachyos\nID_LIKE=arch\n").1), Some(Pm::Pacman));
        assert_eq!(detect(&os_release("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n").1), Some(Pm::Zypper));
    }

    #[test]
    fn plans_use_distro_specific_package_names_without_a_shell() {
        assert_eq!(plan(Pm::Apt, "lm-sensors").unwrap(), ["apt-get", "install", "-y", "lm-sensors"]);
        assert_eq!(plan(Pm::Dnf, "lm-sensors").unwrap().last().unwrap(), "lm_sensors");
        assert_eq!(plan(Pm::Zypper, "lm-sensors").unwrap().last().unwrap(), "sensors");
        assert_eq!(plan(Pm::Pacman, "mangohud").unwrap(), ["pacman", "-S", "--noconfirm", "--needed", "mangohud"]);
    }

    #[test]
    fn rejects_anything_outside_the_catalog() {
        for bad in ["mangohud; rm -rf /", "../x", "", "--help", "mangohud vim"] {
            assert!(plan(Pm::Apt, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn catalog_has_unique_ids_and_a_package_for_every_manager() {
        let mut ids: Vec<_> = CATALOG.iter().map(|a| a.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        assert!(CATALOG.iter().all(|a| a.pkgs.iter().all(|p| !p.is_empty() && !p.contains(' '))));
    }
}
