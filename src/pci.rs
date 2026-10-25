//! Dispositivos PCI: lista (via /sys/bus/pci), nomes (via pci.ids ou lspci) e velocidade do link PCIe.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Serialize;

use crate::util::*;

/// Base de nomes do pci.ids (fabricantes, dispositivos e classes).
#[derive(Debug, Default)]
pub struct PciDb {
    vendors: HashMap<u16, String>,
    devices: HashMap<(u16, u16), String>,
    classes: HashMap<u8, String>,
    subclasses: HashMap<(u8, u8), String>,
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Vendor(u16),
    Class(u8),
}

impl PciDb {
    /// Interpreta o texto do pci.ids.
    pub fn parse(text: &str) -> PciDb {
        let mut db = PciDb::default();
        let mut section = Section::None;
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("C ") {
                if let (Some(code), Some(name)) = (rest.get(..2).and_then(|c| u8::from_str_radix(c, 16).ok()), rest.get(2..)) {
                    db.classes.insert(code, name.trim().to_string());
                    section = Section::Class(code);
                }
            } else if let Some(rest) = line.strip_prefix("\t\t") {
                let _ = rest; // subsistemas e interfaces de programação: não usados
            } else if let Some(rest) = line.strip_prefix('\t') {
                match section {
                    Section::Vendor(v) => {
                        if let (Some(id), Some(name)) = (rest.get(..4).and_then(|c| u16::from_str_radix(c, 16).ok()), rest.get(4..)) {
                            db.devices.insert((v, id), name.trim().to_string());
                        }
                    }
                    Section::Class(c) => {
                        if let (Some(id), Some(name)) = (rest.get(..2).and_then(|c| u8::from_str_radix(c, 16).ok()), rest.get(2..)) {
                            db.subclasses.insert((c, id), name.trim().to_string());
                        }
                    }
                    Section::None => {}
                }
            } else if let (Some(id), Some(name)) = (line.get(..4).and_then(|c| u16::from_str_radix(c, 16).ok()), line.get(4..)) {
                db.vendors.insert(id, name.trim().to_string());
                section = Section::Vendor(id);
            }
        }
        db
    }

    /// Procura o pci.ids nos lugares usuais das distribuições.
    pub fn load() -> Option<PciDb> {
        for p in ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/usr/share/pci.ids", "/var/lib/pciutils/pci.ids", "/usr/local/share/pci.ids"] {
            if let Ok(t) = fs::read_to_string(p) {
                return Some(PciDb::parse(&t));
            }
        }
        None
    }

    /// Banco global carregado uma única vez.
    pub fn global() -> Option<&'static PciDb> {
        static DB: OnceLock<Option<PciDb>> = OnceLock::new();
        DB.get_or_init(PciDb::load).as_ref()
    }

    pub fn vendor(&self, id: u16) -> Option<&str> {
        self.vendors.get(&id).map(String::as_str)
    }
    pub fn device(&self, vendor: u16, device: u16) -> Option<&str> {
        self.devices.get(&(vendor, device)).map(String::as_str)
    }
    pub fn class_name(&self, class: u32) -> Option<&str> {
        let (c, s) = ((class >> 16) as u8, (class >> 8) as u8);
        self.subclasses.get(&(c, s)).or_else(|| self.classes.get(&c)).map(String::as_str)
    }
}

/// Categoria usada pela interface a partir do código de classe PCI.
pub fn category(class: u32) -> &'static str {
    match (class >> 16) as u8 {
        0x01 => "armazenamento",
        0x02 => "rede",
        0x03 => "vídeo",
        0x04 => "áudio/multimídia",
        0x06 => "ponte",
        0x0c => match (class >> 8) as u8 {
            0x03 => "USB",
            _ => "barramento serial",
        },
        0x0d => "sem fio",
        _ => "outro",
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PciDevice {
    pub slot: String,
    pub vendor: u16,
    pub device: u16,
    pub class: u32,
    pub revision: Option<u8>,
    pub driver: Option<String>,
    pub boot_vga: bool,
    pub link_speed: Option<String>,
    pub link_width: Option<u32>,
    pub max_link_speed: Option<String>,
    pub max_link_width: Option<u32>,
    pub vendor_name: String,
    pub device_name: String,
    pub class_name: String,
}

impl PciDevice {
    pub fn id_string(&self) -> String {
        format!("{:04x}:{:04x}", self.vendor, self.device)
    }
    pub fn category(&self) -> &'static str {
        category(self.class)
    }
    /// "Gen4 x16" (link atual) ou `None`.
    pub fn link_now(&self) -> Option<String> {
        link_label(self.link_speed.as_deref(), self.link_width)
    }
    pub fn link_max(&self) -> Option<String> {
        link_label(self.max_link_speed.as_deref(), self.max_link_width)
    }
}

/// Geração do PCIe a partir de "16.0 GT/s PCIe".
pub fn pcie_gen(speed: &str) -> Option<u8> {
    let gt: f64 = speed.split_whitespace().next()?.parse().ok()?;
    Some(match gt as u32 {
        2 => 1,
        5 => 2,
        8 => 3,
        16 => 4,
        32 => 5,
        64 => 6,
        _ => return None,
    })
}

fn link_label(speed: Option<&str>, width: Option<u32>) -> Option<String> {
    let g = pcie_gen(speed?)?;
    Some(format!("Gen{g} x{}", width?))
}

fn hex(s: Option<String>) -> Option<u32> {
    u32::from_str_radix(s?.trim_start_matches("0x"), 16).ok()
}

/// Lista todos os dispositivos PCI com nomes resolvidos.
pub fn list(r: &Roots) -> Vec<PciDevice> {
    let base = r.sys.join("bus/pci/devices");
    let db = if r.sys == Path::new("/sys") { PciDb::global() } else { None };
    let mut v: Vec<PciDevice> = dir_names(&base)
        .into_iter()
        .filter_map(|slot| {
            let d = base.join(&slot);
            let vendor = hex(read_trim(d.join("vendor")))? as u16;
            let device = hex(read_trim(d.join("device")))? as u16;
            let class = hex(read_trim(d.join("class"))).unwrap_or(0);
            let mut dev = PciDevice {
                slot,
                vendor,
                device,
                class,
                revision: hex(read_trim(d.join("revision"))).map(|x| x as u8),
                driver: fs::read_link(d.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
                boot_vga: read_trim(d.join("boot_vga")).as_deref() == Some("1"),
                link_speed: read_trim(d.join("current_link_speed")),
                link_width: read_num(d.join("current_link_width")),
                max_link_speed: read_trim(d.join("max_link_speed")),
                max_link_width: read_num(d.join("max_link_width")),
                ..Default::default()
            };
            if let Some(db) = db {
                dev.vendor_name = db.vendor(vendor).unwrap_or("").to_string();
                dev.device_name = db.device(vendor, device).unwrap_or("").to_string();
                dev.class_name = db.class_name(class).unwrap_or("").to_string();
            }
            Some(dev)
        })
        .collect();
    if db.is_none() && r.sys == Path::new("/sys") && v.iter().any(|d| d.device_name.is_empty()) {
        fill_names_from_lspci(&mut v);
    }
    v
}

/// Alternativa quando não há pci.ids em texto (por exemplo, só o .gz): o `lspci` sabe ler.
fn fill_names_from_lspci(devs: &mut [PciDevice]) {
    let Some(out) = run_stdout("lspci", &["-mm", "-D"], Duration::from_secs(4)) else { return };
    for line in out.lines() {
        let parts = split_quoted(line);
        if parts.len() < 4 {
            continue;
        }
        let slot = parts[0].split_whitespace().next().unwrap_or("");
        if let Some(d) = devs.iter_mut().find(|d| d.slot == slot) {
            d.class_name = parts[1].clone();
            d.vendor_name = parts[2].clone();
            d.device_name = parts[3].clone();
        }
    }
}

/// Divide `0000:01:00.0 "VGA" "NVIDIA" "AD104"` em campos, respeitando as aspas.
fn split_quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    if let Some((first, tail)) = rest.split_once(' ') {
        out.push(first.to_string());
        rest = tail;
    }
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

/// Fabricante abreviado: "NVIDIA Corporation" -> "NVIDIA".
pub fn short_vendor(name: &str) -> String {
    if let Some(b) = bracket_content(name)
        && let Some(first) = b.split('/').next() {
            return first.trim().to_string();
        }
    let n = name.trim();
    for suffix in [" Corporation", " Corp.", ", Inc.", " Inc.", " Co., Ltd.", " Technologies"] {
        if let Some(s) = n.strip_suffix(suffix) {
            return s.trim().to_string();
        }
    }
    n.to_string()
}

fn bracket_content(s: &str) -> Option<&str> {
    let start = s.rfind('[')?;
    let end = s[start..].find(']')? + start;
    Some(&s[start + 1..end])
}

/// Nome de placa de vídeo amigável: "AD104 [GeForce RTX 4070]" + NVIDIA -> "NVIDIA GeForce RTX 4070".
pub fn pretty_device_name(vendor_name: &str, device_name: &str) -> String {
    let vendor = short_vendor(vendor_name);
    let dev = bracket_content(device_name).unwrap_or(device_name).trim();
    if dev.is_empty() { vendor } else if vendor.is_empty() || dev.starts_with(&vendor) { dev.to_string() } else { format!("{vendor} {dev}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# comentario\n\
1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\
\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]\n\
\t\t1002 0e3b  subsistema ignorado\n\
10de  NVIDIA Corporation\n\
\t2786  AD104 [GeForce RTX 4070]\n\
8086  Intel Corporation\n\
\t4680  AlderLake-S GT1 [UHD Graphics 770]\n\
C 02  Network controller\n\
\t00  Ethernet controller\n\
C 03  Display controller\n\
\t00  VGA compatible controller\n\
\t\t00  VGA controller\n\
\t02  3D controller\n";

    #[test]
    fn parses_pci_ids() {
        let db = PciDb::parse(SAMPLE);
        assert_eq!(db.vendor(0x10de), Some("NVIDIA Corporation"));
        assert_eq!(db.device(0x10de, 0x2786), Some("AD104 [GeForce RTX 4070]"));
        assert_eq!(db.device(0x1002, 0x744c), Some("Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]"));
        assert_eq!(db.class_name(0x030000), Some("VGA compatible controller"));
        assert_eq!(db.class_name(0x030200), Some("3D controller"));
        assert_eq!(db.class_name(0x020000), Some("Ethernet controller"));
        assert!(db.vendor(0xdead).is_none());
    }

    #[test]
    fn pretty_names() {
        assert_eq!(pretty_device_name("NVIDIA Corporation", "AD104 [GeForce RTX 4070]"), "NVIDIA GeForce RTX 4070");
        assert_eq!(pretty_device_name("Advanced Micro Devices, Inc. [AMD/ATI]", "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900M]"), "AMD Radeon RX 7900 XT/7900 XTX/7900M");
        assert_eq!(pretty_device_name("Intel Corporation", "AlderLake-S GT1 [UHD Graphics 770]"), "Intel UHD Graphics 770");
        assert_eq!(pretty_device_name("Intel Corporation", ""), "Intel");
        assert_eq!(short_vendor("Realtek Semiconductor Co., Ltd."), "Realtek Semiconductor");
    }

    #[test]
    fn categories_and_pcie_gen() {
        assert_eq!(category(0x030000), "vídeo");
        assert_eq!(category(0x010802), "armazenamento");
        assert_eq!(category(0x0c0330), "USB");
        assert_eq!(pcie_gen("16.0 GT/s PCIe"), Some(4));
        assert_eq!(pcie_gen("2.5 GT/s PCIe"), Some(1));
        assert_eq!(pcie_gen("8.0 GT/s PCIe"), Some(3));
        assert_eq!(pcie_gen("Unknown"), None);
    }

    #[test]
    fn splits_lspci_lines() {
        let f = split_quoted("0000:01:00.0 \"VGA compatible controller\" \"NVIDIA Corporation\" \"AD104 [GeForce RTX 4070]\" -ra1 -p00");
        assert_eq!(f[0], "0000:01:00.0");
        assert_eq!(f[2], "NVIDIA Corporation");
        assert_eq!(f[3], "AD104 [GeForce RTX 4070]");
    }

    #[test]
    fn lists_devices_from_sysfs() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("sys/bus/pci/devices/0000:01:00.0");
        fs::create_dir_all(&dev).unwrap();
        for (f, v) in [("vendor", "0x10de"), ("device", "0x2786"), ("class", "0x030000"), ("revision", "0xa1"), ("boot_vga", "1"), ("current_link_speed", "16.0 GT/s PCIe"), ("current_link_width", "16"), ("max_link_speed", "16.0 GT/s PCIe"), ("max_link_width", "16")] {
            fs::write(dev.join(f), format!("{v}\n")).unwrap();
        }
        let v = list(&Roots::under(d.path()));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id_string(), "10de:2786");
        assert_eq!(v[0].category(), "vídeo");
        assert_eq!(v[0].link_now().as_deref(), Some("Gen4 x16"));
        assert!(v[0].boot_vga);
        assert_eq!(v[0].revision, Some(0xa1));
    }
}
