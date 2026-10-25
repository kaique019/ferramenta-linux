//! Programas de inicialização (XDG autostart): listar, desativar com backup e restaurar.
//!
//! Nada é apagado sem cópia: o arquivo `.desktop` removido vai para a pasta de backup e o registro devolvido pode ser
//! guardado nas configurações para "Restaurar removidos". Entradas do sistema (/etc/xdg/autostart) não são tocadas:
//! elas são desativadas pelo método padrão do XDG, um arquivo do usuário com `Hidden=true`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::settings::StartupBackup;
use crate::util::{User, app_state_dir, config_dir, create_dir_for_user, give_to_user, now_unix, write_atomic};

#[derive(Debug, Clone)]
pub struct Dirs {
    pub user: PathBuf,
    pub system: Vec<PathBuf>,
    pub backup: PathBuf,
}

impl Dirs {
    pub fn system_default() -> Dirs {
        Dirs { user: config_dir().join("autostart"), system: vec![PathBuf::from("/etc/xdg/autostart")], backup: app_state_dir().join("startup-backup") }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AutostartEntry {
    /// Nome do arquivo, como "discord.desktop".
    pub file: String,
    pub name: String,
    pub exec: String,
    pub path: String,
    /// Vem de /etc/xdg (sem ser sobrescrita pelo usuário).
    pub system: bool,
    pub enabled: bool,
}

/// Chaves da seção `[Desktop Entry]` (ignora traduções como `Name[pt_BR]`).
pub fn parse_desktop(text: &str) -> Vec<(String, String)> {
    let mut in_entry = false;
    let mut v = vec![];
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
        } else if in_entry && !l.starts_with('#') {
            if let Some((k, val)) = l.split_once('=') {
                if !k.contains('[') {
                    v.push((k.trim().to_string(), val.trim().to_string()));
                }
            }
        }
    }
    v
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> Option<&'a str> {
    kv.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn is_true(v: Option<&str>) -> bool {
    v.is_some_and(|s| s.eq_ignore_ascii_case("true"))
}

fn read_entry(path: &Path, system: bool) -> Option<AutostartEntry> {
    let file = path.file_name()?.to_string_lossy().into_owned();
    if !file.ends_with(".desktop") {
        return None;
    }
    let kv = parse_desktop(&fs::read_to_string(path).ok()?);
    let hidden = is_true(get(&kv, "Hidden")) || get(&kv, "X-GNOME-Autostart-enabled").is_some_and(|v| v.eq_ignore_ascii_case("false"));
    Some(AutostartEntry { name: get(&kv, "Name").map(str::to_string).unwrap_or_else(|| file.trim_end_matches(".desktop").to_string()), exec: get(&kv, "Exec").unwrap_or("").to_string(), path: path.to_string_lossy().into_owned(), system, enabled: !hidden, file })
}

/// Entradas de inicialização; o arquivo do usuário vale mais que o do sistema com o mesmo nome.
pub fn list(d: &Dirs) -> Vec<AutostartEntry> {
    let mut out: Vec<AutostartEntry> = vec![];
    let mut names: Vec<_> = fs::read_dir(&d.user).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    names.sort();
    out.extend(names.iter().filter_map(|p| read_entry(p, false)));
    for dir in &d.system {
        let mut ps: Vec<_> = fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
        ps.sort();
        for p in ps {
            if let Some(e) = read_entry(&p, true) {
                if !out.iter().any(|o| o.file == e.file) {
                    out.push(e);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn check_name(file: &str) -> Result<(), String> {
    if file.is_empty() || file.contains('/') || file.contains('\0') || file.starts_with('.') || !file.ends_with(".desktop") {
        return Err(format!("nome de arquivo inválido: {file:?}"));
    }
    Ok(())
}

const OVERRIDE: &str = "[Desktop Entry]\nType=Application\nHidden=true\n";

fn is_our_override(path: &Path) -> bool {
    let kv = parse_desktop(&fs::read_to_string(path).unwrap_or_default());
    is_true(get(&kv, "Hidden")) && get(&kv, "Exec").is_none()
}

fn write_user_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(p) = path.parent() {
        create_dir_for_user(p).map_err(|e| format!("não consegui criar {}: {e}", p.display()))?;
    }
    write_atomic(path, contents).map_err(|e| format!("não consegui gravar {}: {e}", path.display()))?;
    give_to_user(path);
    Ok(())
}

/// Desativa a entrada `file`. Devolve o registro para restaurar depois.
pub fn disable(d: &Dirs, file: &str) -> Result<StartupBackup, String> {
    check_name(file)?;
    let user_path = d.user.join(file);
    let in_system = d.system.iter().any(|s| s.join(file).is_file());
    let name = list(d).into_iter().find(|e| e.file == file).map(|e| e.name).ok_or_else(|| format!("{file} não está na inicialização"))?;
    let mut backup = String::new();
    if user_path.is_file() {
        if is_our_override(&user_path) {
            return Err(format!("{file} já está desativado"));
        }
        create_dir_for_user(&d.backup).map_err(|e| format!("não consegui criar a pasta de backup: {e}"))?;
        let b = d.backup.join(format!("{}-{file}", now_unix()));
        fs::copy(&user_path, &b).map_err(|e| format!("falha no backup: {e}"))?;
        give_to_user(&b);
        backup = b.to_string_lossy().into_owned();
        fs::remove_file(&user_path).map_err(|e| format!("não consegui remover {}: {e}", user_path.display()))?;
    }
    if in_system {
        if let Err(e) = write_user_file(&user_path, OVERRIDE.as_bytes()) {
            // desfaz: o arquivo do usuário volta do backup
            if !backup.is_empty() {
                let _ = fs::copy(&backup, &user_path);
            }
            return Err(e);
        }
    } else if backup.is_empty() {
        return Err(format!("{file} não existe"));
    }
    Ok(StartupBackup { name, original: user_path.to_string_lossy().into_owned(), backup, removed_at: now_unix() })
}

/// Desfaz `disable`. Só mexe no caminho original se for o arquivo de bloqueio criado por nós.
pub fn restore(b: &StartupBackup) -> Result<(), String> {
    let original = Path::new(&b.original);
    if original.extension().is_none_or(|e| e != "desktop") {
        return Err(format!("registro inválido: {}", b.original));
    }
    if original.exists() && !is_our_override(original) {
        return Err(format!("{} já existe; não vou sobrescrever", b.original));
    }
    if b.backup.is_empty() {
        // havia só o bloqueio sobre uma entrada do sistema
        if original.exists() {
            fs::remove_file(original).map_err(|e| format!("não consegui remover o bloqueio: {e}"))?;
        }
        return Ok(());
    }
    let data = fs::read(&b.backup).map_err(|e| format!("backup ilegível ({}): {e}", b.backup))?;
    write_user_file(original, &data)?;
    let _ = fs::remove_file(&b.backup);
    Ok(())
}

/// Só para informar de quem é a pasta quando roda sob sudo.
pub fn target_user() -> &'static str {
    &User::target().name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(base: &Path) -> Dirs {
        Dirs { user: base.join("home/autostart"), system: vec![base.join("etc/autostart")], backup: base.join("state/backup") }
    }

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn parses_only_desktop_entry_section_without_translations() {
        let kv = parse_desktop("# c\n[Desktop Entry]\nName=App\nName[pt_BR]=Aplicativo\nExec=app --x=1\n[Desktop Action a]\nExec=outro\n");
        assert_eq!(get(&kv, "Name"), Some("App"));
        assert_eq!(get(&kv, "Exec"), Some("app --x=1"), "valor com '=' preservado e outra seção ignorada");
    }

    #[test]
    fn lists_user_entries_over_system_ones() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        put(&dr.system[0].join("a.desktop"), "[Desktop Entry]\nName=Sistema A\nExec=a\n");
        put(&dr.system[0].join("b.desktop"), "[Desktop Entry]\nName=B\nExec=b\n");
        put(&dr.user.join("a.desktop"), "[Desktop Entry]\nName=Meu A\nExec=a2\nX-GNOME-Autostart-enabled=false\n");
        put(&dr.user.join("leia.txt"), "x");
        let l = list(&dr);
        assert_eq!(l.len(), 2);
        let a = l.iter().find(|e| e.file == "a.desktop").unwrap();
        assert_eq!((a.name.as_str(), a.system, a.enabled), ("Meu A", false, false));
        assert!(l.iter().find(|e| e.file == "b.desktop").unwrap().system);
    }

    #[test]
    fn disable_and_restore_user_entry_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        let text = "[Desktop Entry]\nName=Discord\nExec=discord --start-minimized\n";
        put(&dr.user.join("discord.desktop"), text);
        let b = disable(&dr, "discord.desktop").unwrap();
        assert!(!dr.user.join("discord.desktop").exists());
        assert_eq!(fs::read_to_string(&b.backup).unwrap(), text);
        assert!(list(&dr).is_empty());
        restore(&b).unwrap();
        assert_eq!(fs::read_to_string(dr.user.join("discord.desktop")).unwrap(), text);
        assert!(!Path::new(&b.backup).exists());
    }

    #[test]
    fn system_entry_is_hidden_by_override_and_restored_by_removing_it() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        let sys = dr.system[0].join("tracker.desktop");
        put(&sys, "[Desktop Entry]\nName=Tracker\nExec=tracker\n");
        let b = disable(&dr, "tracker.desktop").unwrap();
        assert!(sys.exists(), "o arquivo do sistema nunca é tocado");
        assert!(!list(&dr)[0].enabled);
        assert!(disable(&dr, "tracker.desktop").is_err(), "já desativado");
        restore(&b).unwrap();
        assert!(!dr.user.join("tracker.desktop").exists());
        assert!(list(&dr)[0].enabled);
    }

    #[test]
    fn user_file_shadowing_system_entry_comes_back_on_restore() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        put(&dr.system[0].join("x.desktop"), "[Desktop Entry]\nName=X\nExec=x\n");
        let mine = "[Desktop Entry]\nName=X personalizado\nExec=x --meu\n";
        put(&dr.user.join("x.desktop"), mine);
        let b = disable(&dr, "x.desktop").unwrap();
        assert!(!list(&dr)[0].enabled);
        restore(&b).unwrap();
        assert_eq!(fs::read_to_string(dr.user.join("x.desktop")).unwrap(), mine);
    }

    #[test]
    fn rejects_path_tricks_and_unknown_entries() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        for bad in ["../x.desktop", "a/b.desktop", "", ".oculto.desktop", "x.sh"] {
            assert!(disable(&dr, bad).is_err(), "{bad:?}");
        }
        assert!(disable(&dr, "nada.desktop").is_err());
        let evil = StartupBackup { name: "x".into(), original: d.path().join("passwd").to_string_lossy().into_owned(), backup: String::new(), removed_at: 0 };
        assert!(restore(&evil).is_err());
    }

    #[test]
    fn restore_does_not_overwrite_a_file_the_user_recreated() {
        let d = tempfile::tempdir().unwrap();
        let dr = dirs(d.path());
        put(&dr.user.join("a.desktop"), "[Desktop Entry]\nName=A\nExec=a\n");
        let b = disable(&dr, "a.desktop").unwrap();
        put(&dr.user.join("a.desktop"), "[Desktop Entry]\nName=Novo\nExec=novo\n");
        assert!(restore(&b).is_err());
        assert!(fs::read_to_string(dr.user.join("a.desktop")).unwrap().contains("Novo"));
        assert!(Path::new(&b.backup).exists(), "o backup continua guardado");
    }
}
