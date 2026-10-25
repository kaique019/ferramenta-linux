//! Portas seriais USB (Arduino, ESP32, conversores CH340/FTDI): lista e diagnostica por que não abrem.
//!
//! Os três culpados clássicos no Linux: o usuário fora do grupo `dialout`/`uucp`, o `brltty` roubando o dispositivo
//! e o `ModemManager` sondando a porta. O diagnóstico é uma função pura sobre fatos já coletados.

use std::path::Path;

use serde::Serialize;

use crate::util::{Roots, dir_names, have, natural_cmp, read_trim, run};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SerialPort {
    /// Ex.: "/dev/ttyUSB0".
    pub device: String,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    /// Grupo dono do dispositivo, quando se sabe.
    pub group: Option<String>,
    /// O usuário atual consegue abrir (ler e escrever).
    pub accessible: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Facts {
    pub user_groups: Vec<String>,
    pub brltty_running: bool,
    pub modemmanager_running: bool,
    pub is_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Advice {
    pub problem: String,
    pub fix: String,
}

/// Procura os arquivos idVendor/idProduct subindo a partir do dispositivo USB do tty.
fn usb_attr(tty_dir: &Path, file: &str) -> Option<String> {
    let mut cur = std::fs::canonicalize(tty_dir.join("device")).ok()?;
    for _ in 0..5 {
        if let Some(v) = read_trim(cur.join(file)) {
            return Some(v);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

pub fn list(r: &Roots) -> Vec<SerialPort> {
    let base = r.sys.join("class/tty");
    let mut names: Vec<String> = dir_names(&base).into_iter().filter(|n| n.starts_with("ttyUSB") || n.starts_with("ttyACM")).collect();
    names.sort_by(|a, b| natural_cmp(a, b));
    names
        .into_iter()
        .map(|n| {
            let d = base.join(&n);
            let device = r.dev.join(&n);
            SerialPort {
                vendor_id: usb_attr(&d, "idVendor"),
                product_id: usb_attr(&d, "idProduct"),
                product: usb_attr(&d, "product"),
                manufacturer: usb_attr(&d, "manufacturer"),
                group: device_group(&device),
                accessible: crate::util::can_read_write(&device),
                device: device.to_string_lossy().into_owned(),
            }
        })
        .collect()
}

fn device_group(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let gid = std::fs::metadata(path).ok()?.gid();
    // Resolve o nome do grupo em /etc/group
    std::fs::read_to_string("/etc/group").ok()?.lines().find_map(|l| {
        let mut p = l.split(':');
        let (name, _, id) = (p.next()?, p.next()?, p.next()?);
        (id.parse::<u32>().ok()? == gid).then(|| name.to_string())
    })
}

fn active(unit: &str) -> bool {
    run("systemctl", &["is-active", "--quiet", unit], Duration::from_secs(3)).is_some_and(|o| o.ok())
}

pub fn gather_facts() -> Facts {
    Facts {
        user_groups: crate::util::groups_now(),
        brltty_running: have("systemctl") && (active("brltty") || active("brltty.path")) || run("pgrep", &["-x", "brltty"], Duration::from_secs(2)).is_some_and(|o| o.ok()),
        modemmanager_running: have("systemctl") && active("ModemManager"),
        is_root: crate::util::is_root(),
    }
}

/// O que está impedindo (ou pode impedir) o uso das portas, com a correção.
pub fn diagnose(ports: &[SerialPort], f: &Facts) -> Vec<Advice> {
    let mut v = vec![];
    if ports.is_empty() {
        let mut fix = "Troque o cabo (alguns só carregam, sem dados) e a porta USB; confira com `dmesg | tail`.".to_string();
        if f.brltty_running {
            fix.push_str(" O brltty está ativo e costuma tomar adaptadores CH340/CP210x: veja o item abaixo.");
        }
        v.push(Advice { problem: "Nenhuma porta serial USB encontrada".into(), fix });
    }
    let blocked: Vec<&SerialPort> = ports.iter().filter(|p| !p.accessible).collect();
    if !blocked.is_empty() && !f.is_root {
        let groups: Vec<String> = blocked.iter().filter_map(|p| p.group.clone()).collect();
        let group = groups.first().cloned().unwrap_or_else(|| "dialout".into());
        let member = f.user_groups.iter().any(|g| *g == group);
        v.push(if member {
            Advice { problem: format!("Você está no grupo {group}, mas a sessão ainda não aplicou"), fix: "Saia e entre de novo na sessão (ou reinicie) e tente outra vez.".into() }
        } else {
            Advice { problem: format!("Sem permissão em {}", blocked.iter().map(|p| p.device.as_str()).collect::<Vec<_>>().join(", ")), fix: format!("sudo usermod -aG {group} $USER   (depois saia e entre na sessão)") }
        });
    }
    if f.brltty_running {
        v.push(Advice { problem: "O brltty (leitor de braille) pode estar tomando a porta".into(), fix: "sudo systemctl disable --now brltty brltty.path   (ou: sudo apt remove brltty)".into() });
    }
    if f.modemmanager_running {
        v.push(Advice { problem: "O ModemManager pode sondar a porta e atrapalhar o upload".into(), fix: "sudo systemctl disable --now ModemManager   (ou crie uma regra udev ID_MM_DEVICE_IGNORE=1 para o seu adaptador)".into() });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn port(accessible: bool, group: Option<&str>) -> SerialPort {
        SerialPort { device: "/dev/ttyUSB0".into(), group: group.map(str::to_string), accessible, ..Default::default() }
    }

    #[test]
    fn lists_only_usb_serial_ttys_with_usb_ids() {
        let d = tempfile::tempdir().unwrap();
        let usb = d.path().join("sys/devices/pci/usb1/1-1");
        fs::create_dir_all(usb.join("1-1:1.0/ttyUSB0")).unwrap();
        fs::write(usb.join("idVendor"), "1a86\n").unwrap();
        fs::write(usb.join("idProduct"), "7523\n").unwrap();
        fs::write(usb.join("product"), "USB Serial\n").unwrap();
        let tty = d.path().join("sys/class/tty/ttyUSB0");
        fs::create_dir_all(&tty).unwrap();
        std::os::unix::fs::symlink(usb.join("1-1:1.0"), tty.join("device")).unwrap();
        fs::create_dir_all(d.path().join("sys/class/tty/tty0")).unwrap();
        fs::create_dir_all(d.path().join("sys/class/tty/ttyS0")).unwrap();
        let v = list(&Roots::under(d.path()));
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].vendor_id.as_deref(), v[0].product_id.as_deref(), v[0].product.as_deref()), (Some("1a86"), Some("7523"), Some("USB Serial")));
        assert!(!v[0].accessible, "o /dev falso não existe");
    }

    #[test]
    fn suggests_group_when_not_a_member() {
        let f = Facts { user_groups: vec!["users".into()], ..Default::default() };
        let a = diagnose(&[port(false, Some("dialout"))], &f);
        assert_eq!(a.len(), 1);
        assert!(a[0].fix.contains("usermod -aG dialout"));
    }

    #[test]
    fn member_of_group_is_told_to_relogin() {
        let f = Facts { user_groups: vec!["dialout".into()], ..Default::default() };
        let a = diagnose(&[port(false, Some("dialout"))], &f);
        assert!(a[0].problem.contains("sessão ainda não aplicou"));
    }

    #[test]
    fn root_and_accessible_ports_need_no_group_advice() {
        assert!(diagnose(&[port(true, Some("dialout"))], &Facts::default()).is_empty());
        assert!(diagnose(&[port(false, Some("dialout"))], &Facts { is_root: true, ..Default::default() }).is_empty());
    }

    #[test]
    fn flags_brltty_and_modemmanager_and_missing_ports() {
        let f = Facts { brltty_running: true, modemmanager_running: true, ..Default::default() };
        let a = diagnose(&[], &f);
        assert_eq!(a.len(), 3);
        assert!(a[0].fix.contains("brltty"));
        assert!(a.iter().any(|x| x.problem.contains("ModemManager")));
    }
}
