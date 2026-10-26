//! Ajustes de sistema para jogos (sysctl e THP) com restauração garantida.
//!
//! Só existe uma lista fixa de ajustes, cada um com um único valor recomendado; nenhum caminho ou valor vem do
//! usuário. O valor original é guardado antes da primeira alteração (e nunca sobrescrito por aplicações repetidas),
//! então "restaurar" sempre devolve o que o sistema tinha. As mudanças valem até reiniciar.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::util::{Roots, read_trim, write_priv};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Proc,
    Sys,
}

struct Def {
    id: &'static str,
    title: &'static str,
    why: &'static str,
    base: Base,
    file: &'static str,
    recommended: &'static str,
}

const DEFS: &[Def] = &[
    Def { id: "swappiness", title: "Swappiness baixo (10)", why: "Usa menos o swap e mantém jogos na RAM.", base: Base::Proc, file: "sys/vm/swappiness", recommended: "10" },
    Def { id: "max_map_count", title: "max_map_count alto (1048576)", why: "Vários jogos via Proton/Wine (e o Steam Deck) exigem; evita travar ao abrir.", base: Base::Proc, file: "sys/vm/max_map_count", recommended: "1048576" },
    Def { id: "vfs_cache_pressure", title: "Cache de arquivos mais duradouro (50)", why: "Mantém metadados de disco em cache, ajudando no carregamento de mapas.", base: Base::Proc, file: "sys/vm/vfs_cache_pressure", recommended: "50" },
    Def { id: "thp", title: "Transparent Hugepages em madvise", why: "Evita travadinhas de compactação de memória; apps que pedem hugepages ainda recebem.", base: Base::Sys, file: "kernel/mm/transparent_hugepage/enabled", recommended: "madvise" },
];

#[derive(Debug, Clone, Serialize)]
pub struct TweakStatus {
    pub id: &'static str,
    pub title: &'static str,
    pub why: &'static str,
    pub recommended: &'static str,
    /// Valor atual; `None` se este kernel não tem o controle.
    pub current: Option<String>,
    pub available: bool,
    /// Já está no valor recomendado.
    pub applied: bool,
    /// Existe valor original guardado para restaurar.
    pub can_restore: bool,
}

fn path(r: &Roots, d: &Def) -> PathBuf {
    match d.base {
        Base::Proc => r.proc.join(d.file),
        Base::Sys => r.sys.join(d.file),
    }
}

/// Valor atual e opções. Arquivos como o do THP mostram "always [madvise] never".
fn read_value(r: &Roots, d: &Def) -> Option<(String, Vec<String>)> {
    let raw = read_trim(path(r, d))?;
    if raw.contains('[') {
        let opts: Vec<String> = raw.split_whitespace().map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string()).collect();
        let cur = raw.split_whitespace().find(|w| w.starts_with('['))?.trim_matches(|c| c == '[' || c == ']').to_string();
        Some((cur, opts))
    } else {
        Some((raw, vec![]))
    }
}

pub fn status(r: &Roots, backup: &BTreeMap<String, Option<String>>) -> Vec<TweakStatus> {
    DEFS.iter()
        .map(|d| {
            let v = read_value(r, d);
            TweakStatus {
                id: d.id,
                title: d.title,
                why: d.why,
                recommended: d.recommended,
                applied: v.as_ref().is_some_and(|(c, _)| c == d.recommended),
                available: v.as_ref().is_some_and(|(_, o)| o.is_empty() || o.iter().any(|x| x == d.recommended)),
                current: v.map(|(c, _)| c),
                can_restore: backup.contains_key(d.id),
            }
        })
        .collect()
}

fn find(id: &str) -> Result<&'static Def, String> {
    DEFS.iter().find(|d| d.id == id).ok_or_else(|| format!("ajuste desconhecido: {id} (opções: {})", DEFS.iter().map(|d| d.id).collect::<Vec<_>>().join(", ")))
}

/// Aplica o valor recomendado, guardando o original em `backup` (só na primeira vez).
pub fn apply(r: &Roots, backup: &mut BTreeMap<String, Option<String>>, id: &str) -> Result<(), String> {
    let d = find(id)?;
    let (cur, opts) = read_value(r, d).ok_or_else(|| format!("este kernel não tem o controle de {id}"))?;
    if !opts.is_empty() && !opts.iter().any(|o| o == d.recommended) {
        return Err(format!("este kernel não aceita {} em {id} (opções: {})", d.recommended, opts.join(", ")));
    }
    let first = !backup.contains_key(d.id);
    if first {
        backup.insert(d.id.to_string(), Some(encode(&boot_id(r), &cur)));
    }
    if let Err(e) = write_priv(&path(r, d), d.recommended) {
        if first {
            backup.remove(d.id); // nada mudou: não guarda um "original" inútil
        }
        return Err(e);
    }
    Ok(())
}

/// O valor original é guardado junto com o id do boot: depois de reiniciar, o sistema já voltou ao padrão dele
/// (que pode ter mudado, por exemplo via sysctl.conf), então o valor antigo não deve ser regravado.
fn boot_id(r: &Roots) -> String {
    read_trim(r.proc.join("sys/kernel/random/boot_id")).unwrap_or_default()
}

fn encode(boot: &str, value: &str) -> String {
    format!("{boot}\t{value}")
}

/// (boot, valor). Registros antigos, sem boot, valem como "mesmo boot".
fn decode(stored: &str) -> (Option<&str>, &str) {
    match stored.split_once('\t') {
        Some((b, v)) => (Some(b), v),
        None => (None, stored),
    }
}

/// O valor guardado precisa ter o formato do ajuste: o `settings.json` é do usuário e o valor vai para /proc/sys como
/// root, então nada que não seja um número (ou uma das opções que o kernel lista) é aceito.
fn plausible(opts: &[String], v: &str) -> bool {
    if opts.is_empty() { !v.is_empty() && v.len() <= 12 && v.bytes().all(|b| b.is_ascii_digit()) } else { opts.iter().any(|o| o == v) }
}

/// Devolve o valor original. Só esquece o backup depois que a escrita funcionou.
/// `Ok(true)` = valor regravado; `Ok(false)` = o computador reiniciou desde a alteração, então já voltou ao padrão.
pub fn restore(r: &Roots, backup: &mut BTreeMap<String, Option<String>>, id: &str) -> Result<bool, String> {
    let d = find(id)?;
    match backup.get(d.id).cloned() {
        None => Err(format!("não há valor original guardado de {id}")),
        Some(None) => {
            backup.remove(d.id);
            Ok(false)
        }
        Some(Some(stored)) => {
            let (boot, orig) = decode(&stored);
            if boot.is_some_and(|b| b != boot_id(r)) {
                backup.remove(d.id);
                return Ok(false);
            }
            let opts = read_value(r, d).map(|(_, o)| o).unwrap_or_default();
            if !plausible(&opts, orig) {
                return Err(format!("o valor guardado de {id} ({orig:?}) não é válido; não vou gravá-lo"));
            }
            write_priv(&path(r, d), orig)?;
            backup.remove(d.id);
            Ok(true)
        }
    }
}

pub fn ids() -> Vec<&'static str> {
    DEFS.iter().map(|d| d.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> (tempfile::TempDir, Roots) {
        let d = tempfile::tempdir().unwrap();
        for (f, v) in [("proc/sys/vm/swappiness", "60\n"), ("proc/sys/vm/max_map_count", "65530\n"), ("sys/kernel/mm/transparent_hugepage/enabled", "[always] madvise never\n")] {
            let p = d.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, v).unwrap();
        }
        let r = Roots::under(d.path());
        (d, r)
    }

    fn put(p: &std::path::Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn val(d: &tempfile::TempDir, f: &str) -> String {
        fs::read_to_string(d.path().join(f)).unwrap().trim().to_string()
    }

    #[test]
    fn status_reads_values_and_marks_missing_controls() {
        let (_d, r) = setup();
        let s = status(&r, &BTreeMap::new());
        let get = |id: &str| s.iter().find(|t| t.id == id).unwrap();
        assert_eq!(get("swappiness").current.as_deref(), Some("60"));
        assert!(!get("swappiness").applied && get("swappiness").available);
        assert_eq!(get("thp").current.as_deref(), Some("always"));
        assert!(get("vfs_cache_pressure").current.is_none() && !get("vfs_cache_pressure").available);
    }

    #[test]
    fn apply_saves_original_once_and_restore_brings_it_back() {
        let (d, r) = setup();
        let mut b = BTreeMap::new();
        apply(&r, &mut b, "swappiness").unwrap();
        assert_eq!(val(&d, "proc/sys/vm/swappiness"), "10");
        assert_eq!(decode(b["swappiness"].as_deref().unwrap()).1, "60");
        let first = b["swappiness"].clone();
        apply(&r, &mut b, "swappiness").unwrap();
        assert_eq!(b["swappiness"], first, "aplicar de novo não perde o original");
        assert_eq!(restore(&r, &mut b, "swappiness"), Ok(true));
        assert_eq!(val(&d, "proc/sys/vm/swappiness"), "60");
        assert!(b.is_empty());
        assert!(restore(&r, &mut b, "swappiness").is_err());
    }

    #[test]
    fn restore_skips_the_write_after_a_reboot() {
        let (d, r) = setup();
        put(&r.proc.join("sys/kernel/random/boot_id"), "boot-A\n");
        let mut b = BTreeMap::new();
        apply(&r, &mut b, "swappiness").unwrap();
        assert_eq!(val(&d, "proc/sys/vm/swappiness"), "10");
        // reinicia: novo boot, o sistema voltou ao padrão dele (aqui, 30, diferente do "original" guardado)
        put(&r.proc.join("sys/kernel/random/boot_id"), "boot-B\n");
        std::fs::write(d.path().join("proc/sys/vm/swappiness"), "30\n").unwrap();
        assert_eq!(restore(&r, &mut b, "swappiness"), Ok(false));
        assert_eq!(val(&d, "proc/sys/vm/swappiness"), "30", "o valor do novo boot não é sobrescrito");
        assert!(b.is_empty());
    }

    #[test]
    fn restore_rejects_tampered_backup_values() {
        let (d, r) = setup();
        for bad in ["abc", "1; echo x", "", "99999999999999999", "../../etc", "-1"] {
            let mut b = BTreeMap::new();
            b.insert("swappiness".to_string(), Some(bad.to_string()));
            assert!(restore(&r, &mut b, "swappiness").is_err(), "{bad:?}");
            assert_eq!(val(&d, "proc/sys/vm/swappiness"), "60", "{bad:?} não pode ser gravado");
            assert!(b.contains_key("swappiness"), "o registro fica para o usuário ver");
        }
        let mut b = BTreeMap::new();
        b.insert("thp".to_string(), Some("hack".to_string()));
        assert!(restore(&r, &mut b, "thp").is_err());
        assert_eq!(val(&d, "sys/kernel/mm/transparent_hugepage/enabled"), "[always] madvise never");
        // valor legítimo no formato antigo (sem boot) continua valendo
        let mut b = BTreeMap::new();
        b.insert("swappiness".to_string(), Some("40".to_string()));
        assert_eq!(restore(&r, &mut b, "swappiness"), Ok(true));
        assert_eq!(val(&d, "proc/sys/vm/swappiness"), "40");
    }

    #[test]
    fn thp_is_validated_against_kernel_options() {
        let (d, r) = setup();
        let mut b = BTreeMap::new();
        apply(&r, &mut b, "thp").unwrap();
        assert_eq!(val(&d, "sys/kernel/mm/transparent_hugepage/enabled"), "madvise");
        fs::write(d.path().join("sys/kernel/mm/transparent_hugepage/enabled"), "[always] never\n").unwrap();
        let mut b2 = BTreeMap::new();
        assert!(apply(&r, &mut b2, "thp").is_err());
        assert!(b2.is_empty());
    }

    #[test]
    fn unknown_ids_and_missing_controls_are_errors_without_side_effects() {
        let (_d, r) = setup();
        let mut b = BTreeMap::new();
        assert!(apply(&r, &mut b, "../../etc/passwd").is_err());
        assert!(apply(&r, &mut b, "vfs_cache_pressure").is_err());
        assert!(b.is_empty());
    }

    #[test]
    fn failed_write_does_not_leave_a_backup() {
        let (d, r) = setup();
        let p = d.path().join("proc/sys/vm/swappiness");
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap(); // escrever em um diretório falha, e ler também: vira "sem controle"
        let mut b = BTreeMap::new();
        assert!(apply(&r, &mut b, "swappiness").is_err());
        assert!(b.is_empty());
    }
}
