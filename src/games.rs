//! Detecção de jogos instalados: Steam (VDF/ACF), Heroic (Epic, GOG, avulsos), Lutris e jogos adicionados à mão.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::settings::CustomGame;
use crate::util::{as_user, dir_names, read_small};
use crate::vdf;

/// Limites de leitura: estes arquivos ficam na pasta do usuário e podem ser trocados por symlinks.
const MAX_TEXT: u64 = 1 << 20;
const MAX_JSON: u64 = 16 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    Steam,
    Epic,
    Gog,
    Heroic,
    Lutris,
    Custom,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Steam => "Steam",
            Source::Epic => "Epic (Heroic)",
            Source::Gog => "GOG (Heroic)",
            Source::Heroic => "Heroic",
            Source::Lutris => "Lutris",
            Source::Custom => "Manual",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Game {
    /// Identificador estável, como "steam:730" ou "lutris:celeste". Chave dos perfis.
    pub id: String,
    pub name: String,
    pub source: Source,
    pub install_dir: Option<String>,
    pub exe: Option<String>,
    pub size: Option<u64>,
    pub steam_appid: Option<u32>,
}

/// Ferramentas da Steam que aparecem como "jogos" mas não são.
fn is_steam_tool(name: &str, appid: u32) -> bool {
    let n = name.to_lowercase();
    n.starts_with("proton") || n.starts_with("steam linux runtime") || n.starts_with("steamworks common") || n.starts_with("steam controller") || appid == 228980
}

fn steam_roots(home: &Path) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for rel in [".steam/steam", ".local/share/Steam", ".var/app/com.valvesoftware.Steam/.local/share/Steam", ".steam/root"] {
        let p = home.join(rel);
        if let Ok(c) = fs::canonicalize(&p)
            && c.join("steamapps").is_dir() && seen.insert(c.clone()) {
                out.push(c);
            }
    }
    out
}

/// Bibliotecas da Steam: as pastas `steamapps` de cada raiz e as listadas em `libraryfolders.vdf`.
pub fn steam_libraries(home: &Path) -> Vec<PathBuf> {
    as_user(|| steam_libraries_inner(home)).unwrap_or_default()
}

fn steam_libraries_inner(home: &Path) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    let mut add = |p: PathBuf, out: &mut Vec<PathBuf>| {
        let key = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if p.is_dir() && seen.insert(key) {
            out.push(p);
        }
    };
    for root in steam_roots(home) {
        add(root.join("steamapps"), &mut out);
        if let Ok(text) = read_small(root.join("steamapps/libraryfolders.vdf"), MAX_TEXT).ok_or(())
            && let Ok(v) = vdf::parse(&text)
                && let Some(lf) = v.get("libraryfolders") {
                    for (_, entry) in lf.entries() {
                        if let Some(path) = entry.get_str("path") {
                            add(Path::new(path).join("steamapps"), &mut out);
                        }
                    }
                }
    }
    out
}

pub fn steam_games(home: &Path) -> Vec<Game> {
    as_user(|| steam_games_inner(home)).unwrap_or_default()
}

fn steam_games_inner(home: &Path) -> Vec<Game> {
    let mut games = vec![];
    for lib in steam_libraries(home) {
        for name in dir_names(&lib) {
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Some(text) = read_small(lib.join(&name), MAX_TEXT) else { continue };
            let Ok(v) = vdf::parse(&text) else { continue };
            let Some(app) = v.get("AppState") else { continue };
            let (Some(appid), Some(title)) = (app.get_str("appid").and_then(|s| s.parse::<u32>().ok()), app.get_str("name")) else { continue };
            if is_steam_tool(title, appid) {
                continue;
            }
            let dir = app.get_str("installdir").filter(|d| !d.is_empty() && !d.contains("..") && !d.starts_with('/')).map(|d| lib.join("common").join(d));
            games.push(Game {
                id: format!("steam:{appid}"),
                name: title.to_string(),
                source: Source::Steam,
                install_dir: dir.map(|d| d.to_string_lossy().into_owned()),
                exe: None,
                size: app.get_str("SizeOnDisk").and_then(|s| s.parse().ok()),
                steam_appid: Some(appid),
            });
        }
    }
    games
}

fn json_file(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&read_small(path, MAX_JSON)?).ok()
}

fn s(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

pub fn heroic_games(home: &Path) -> Vec<Game> {
    as_user(|| heroic_games_inner(home)).unwrap_or_default()
}

/// Pastas de configuração do Heroic: instalação normal, Flatpak, Snap e `$XDG_CONFIG_HOME`.
fn heroic_bases(home: &Path) -> Vec<PathBuf> {
    let mut v = vec![home.join(".config/heroic"), home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic"), home.join("snap/heroic/current/.config/heroic")];
    if home == crate::util::home_dir() {
        v.push(crate::util::config_dir().join("heroic"));
    }
    let mut seen = vec![];
    v.retain(|p| {
        let fresh = !seen.contains(p);
        seen.push(p.clone());
        fresh
    });
    v
}

/// `executable` da Epic vem relativo à pasta de instalação.
fn absolute_exe(dir: Option<&str>, exe: Option<String>) -> Option<String> {
    let exe = exe?;
    if Path::new(&exe).is_absolute() {
        return Some(exe);
    }
    match dir {
        Some(d) if !exe.split('/').any(|c| c == "..") => Some(Path::new(d).join(exe.trim_start_matches("./")).to_string_lossy().into_owned()),
        _ => Some(exe),
    }
}

/// Aceita a lista tanto em `{ "<chave>": [..] }` quanto solta no topo do arquivo.
fn json_list(v: Option<serde_json::Value>, key: &str) -> Vec<serde_json::Value> {
    match v {
        Some(serde_json::Value::Array(a)) => a,
        Some(o) => o.get(key).and_then(|x| x.as_array().cloned()).unwrap_or_default(),
        None => vec![],
    }
}

fn heroic_games_inner(home: &Path) -> Vec<Game> {
    let mut games: Vec<Game> = vec![];
    let mut push = |g: Game| {
        if !games.iter().any(|x| x.id == g.id) {
            games.push(g);
        }
    };
    for base in heroic_bases(home) {
        if !base.is_dir() {
            continue;
        }
        // Epic: objeto { app_name: { title, install_path, executable, install_size } }
        if let Some(serde_json::Value::Object(m)) = json_file(&base.join("legendaryConfig/legendary/installed.json")) {
            for (app, g) in &m {
                let dir = s(g, "install_path");
                let exe = absolute_exe(dir.as_deref(), s(g, "executable"));
                push(Game { id: format!("epic:{app}"), name: s(g, "title").unwrap_or_else(|| app.clone()), source: Source::Epic, install_dir: dir, exe, size: g.get("install_size").and_then(|v| v.as_u64()), steam_appid: None });
            }
        }
        // GOG: { installed: [ { appName, install_path } ] }, com os títulos em outro arquivo
        let titles = json_list(json_file(&base.join("store_cache/gog_library.json")), "games");
        for g in json_list(json_file(&base.join("gog_store/installed.json")), "installed") {
            let Some(app) = s(&g, "appName").or_else(|| s(&g, "app_name")) else { continue };
            let title = titles.iter().find(|x| s(x, "app_name").or_else(|| s(x, "appName")).as_deref() == Some(&app)).and_then(|x| s(x, "title"));
            push(Game { id: format!("gog:{app}"), name: title.unwrap_or_else(|| app.clone()), source: Source::Gog, install_dir: s(&g, "install_path"), exe: None, size: None, steam_appid: None });
        }
        // Avulsos: { games: [ { app_name, title, folder_name, install: { executable } } ] }
        for g in json_list(json_file(&base.join("sideload_apps/library.json")), "games") {
            let Some(app) = s(&g, "app_name") else { continue };
            // só os instalados (is_installed ausente conta como instalado: versões antigas não tinham o campo)
            if g.get("is_installed").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }
            let dir = s(&g, "folder_name");
            let exe = absolute_exe(dir.as_deref(), g.get("install").and_then(|i| s(i, "executable")));
            push(Game { id: format!("heroic:{app}"), name: s(&g, "title").unwrap_or_else(|| app.clone()), source: Source::Heroic, install_dir: dir, exe, size: None, steam_appid: None });
        }
    }
    games
}

/// Como abrir um jogo do Heroic: pelo próprio Heroic (`heroic://launch/<runner>/<app>`), que cuida do
/// Wine/Proton e do prefixo. Devolve (programa, argumentos), ou `None` se não é jogo do Heroic ou o
/// Heroic não está instalado.
pub fn heroic_launch(g: &Game, home: &Path, have: &dyn Fn(&str) -> bool) -> Option<(String, String)> {
    let runner = match g.source {
        Source::Epic => "legendary",
        Source::Gog => "gog",
        Source::Heroic => "sideload",
        _ => return None,
    };
    let app = g.id.split_once(':')?.1;
    if app.is_empty() || !app.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return None;
    }
    let url = format!("heroic://launch/{runner}/{app}");
    let flatpak = home.join(".var/app/com.heroicgameslauncher.hgl").is_dir();
    if have("heroic") {
        Some(("heroic".into(), format!("--no-gui --no-sandbox {url}")))
    } else if flatpak && have("flatpak") {
        Some(("flatpak".into(), format!("run com.heroicgameslauncher.hgl --no-gui --no-sandbox {url}")))
    } else {
        None
    }
}

/// Lê o essencial de um `.yml` do Lutris (campos simples; não precisa de um parser YAML completo).
fn lutris_game(slug: &str, text: &str) -> Option<Game> {
    let (mut name, mut exe, mut in_game) = (None, None, false);
    for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_game = line.trim_end() == "game:";
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
            }
        } else if in_game
            && let Some(v) = line.trim().strip_prefix("exe:") {
                exe = Some(v.trim().trim_matches(|c| c == '"' || c == '\'').to_string());
            }
    }
    let exe = exe.filter(|e| !e.is_empty());
    Some(Game { id: format!("lutris:{slug}"), name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| slug.to_string()), source: Source::Lutris, install_dir: exe.as_ref().and_then(|e| Path::new(e).parent().map(|p| p.to_string_lossy().into_owned())), exe, size: None, steam_appid: None })
}

pub fn lutris_games(home: &Path) -> Vec<Game> {
    as_user(|| lutris_games_inner(home)).unwrap_or_default()
}

fn lutris_games_inner(home: &Path) -> Vec<Game> {
    let mut out = vec![];
    for rel in [".local/share/lutris/games", ".var/app/net.lutris.Lutris/data/lutris/games"] {
        let dir = home.join(rel);
        for f in dir_names(&dir) {
            if let Some(slug) = f.strip_suffix(".yml")
                && let Some(g) = read_small(dir.join(&f), MAX_TEXT).and_then(|t| lutris_game(slug, &t)) {
                    out.push(g);
                }
        }
    }
    out
}

pub fn custom_games(custom: &[CustomGame]) -> Vec<Game> {
    custom.iter().enumerate().map(|(i, c)| Game { id: format!("custom:{i}:{}", c.name), name: c.name.clone(), source: Source::Custom, install_dir: Path::new(&c.exe).parent().map(|p| p.to_string_lossy().into_owned()), exe: Some(c.exe.clone()), size: None, steam_appid: None }).collect()
}

/// Todos os jogos, em ordem alfabética.
pub fn detect_all(home: &Path, custom: &[CustomGame]) -> Vec<Game> {
    let mut v = steam_games(home);
    v.extend(heroic_games(home));
    v.extend(lutris_games(home));
    v.extend(custom_games(custom));
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(base: &Path, rel: &str, text: &str) {
        let p = base.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn acf(appid: u32, name: &str, dir: &str) -> String {
        format!("\"AppState\"\n{{\n\t\"appid\"\t\t\"{appid}\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{dir}\"\n\t\"SizeOnDisk\"\t\t\"1000\"\n}}\n")
    }

    #[test]
    fn finds_steam_games_across_libraries_and_skips_tools() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path();
        let lib2 = h.join("jogos");
        fs::create_dir_all(lib2.join("steamapps")).unwrap();
        put(h, ".local/share/Steam/steamapps/libraryfolders.vdf", &format!("\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} \"1\" {{ \"path\" \"{}\" }} }}", h.join(".local/share/Steam").display(), lib2.display()));
        put(h, ".local/share/Steam/steamapps/appmanifest_730.acf", &acf(730, "Counter-Strike 2", "Counter-Strike Global Offensive"));
        put(h, ".local/share/Steam/steamapps/appmanifest_1493710.acf", &acf(1493710, "Proton Experimental", "Proton - Experimental"));
        put(h, ".local/share/Steam/steamapps/appmanifest_228980.acf", &acf(228980, "Steamworks Common Redistributables", "x"));
        put(&lib2, "steamapps/appmanifest_620.acf", &acf(620, "Portal 2", "Portal 2"));
        put(&lib2, "steamapps/appmanifest_666.acf", &acf(666, "Evil", "../../etc"));
        put(&lib2, "steamapps/appmanifest_bad.acf", "{{{");
        let g = steam_games(h);
        let names: Vec<_> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"Counter-Strike 2") && names.contains(&"Portal 2"));
        let cs = g.iter().find(|g| g.steam_appid == Some(730)).unwrap();
        assert_eq!(cs.id, "steam:730");
        assert!(cs.install_dir.as_deref().unwrap().ends_with("steamapps/common/Counter-Strike Global Offensive"));
        assert_eq!(cs.size, Some(1000));
        assert!(g.iter().find(|g| g.steam_appid == Some(666)).unwrap().install_dir.is_none(), "installdir com .. é descartado");
    }

    #[test]
    fn same_library_reached_two_ways_is_listed_once() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path();
        put(h, ".local/share/Steam/steamapps/appmanifest_1.acf", &acf(1, "Jogo", "Jogo"));
        std::os::unix::fs::symlink(h.join(".local/share/Steam"), h.join(".steam-link")).unwrap();
        fs::create_dir_all(h.join(".steam")).unwrap();
        std::os::unix::fs::symlink(h.join(".local/share/Steam"), h.join(".steam/steam")).unwrap();
        assert_eq!(steam_games(h).len(), 1);
    }

    #[test]
    fn heroic_is_found_in_flatpak_and_snap_and_epic_exe_becomes_absolute() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path();
        put(h, ".var/app/com.heroicgameslauncher.hgl/config/heroic/legendaryConfig/legendary/installed.json", r#"{"Sugar":{"title":"Celeste","install_path":"/g/celeste","executable":"Celeste.exe"}}"#);
        put(h, "snap/heroic/current/.config/heroic/gog_store/installed.json", r#"[{"appName":"7","install_path":"/g/x"}]"#);
        let g = heroic_games(h);
        let celeste = g.iter().find(|g| g.id == "epic:Sugar").unwrap();
        assert_eq!(celeste.exe.as_deref(), Some("/g/celeste/Celeste.exe"));
        assert!(g.iter().any(|g| g.id == "gog:7"), "GOG com lista solta no topo do arquivo");
    }

    #[test]
    fn uninstalled_sideload_apps_are_skipped_and_ids_are_not_duplicated() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path();
        put(h, ".config/heroic/sideload_apps/library.json", r#"{"games":[{"app_name":"a","title":"A","is_installed":false},{"app_name":"b","title":"B","is_installed":true,"folder_name":"/g/b","install":{"executable":"b.sh"}}]}"#);
        let g = heroic_games(h);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].exe.as_deref(), Some("/g/b/b.sh"));
    }

    #[test]
    fn heroic_games_launch_through_heroic_and_reject_odd_ids() {
        let d = tempfile::tempdir().unwrap();
        let mk = |id: &str, source| Game { id: id.into(), name: "x".into(), source, install_dir: None, exe: None, size: None, steam_appid: None };
        let yes = |_: &str| true;
        let no = |_: &str| false;
        let (exe, args) = heroic_launch(&mk("epic:Sugar", Source::Epic), d.path(), &yes).unwrap();
        assert_eq!((exe.as_str(), args.as_str()), ("heroic", "--no-gui --no-sandbox heroic://launch/legendary/Sugar"));
        assert!(heroic_launch(&mk("gog:123", Source::Gog), d.path(), &yes).unwrap().1.ends_with("heroic://launch/gog/123"));
        assert!(heroic_launch(&mk("heroic:x1", Source::Heroic), d.path(), &yes).unwrap().1.ends_with("heroic://launch/sideload/x1"));
        assert!(heroic_launch(&mk("steam:620", Source::Steam), d.path(), &yes).is_none());
        assert!(heroic_launch(&mk("epic:a b;rm", Source::Epic), d.path(), &yes).is_none(), "id com caracteres estranhos não vira comando");
        assert!(heroic_launch(&mk("epic:Sugar", Source::Epic), d.path(), &no).is_none(), "sem Heroic instalado");
        // Flatpak: só o flatpak existe
        std::fs::create_dir_all(d.path().join(".var/app/com.heroicgameslauncher.hgl")).unwrap();
        let only_flatpak = |c: &str| c == "flatpak";
        let (exe, args) = heroic_launch(&mk("epic:Sugar", Source::Epic), d.path(), &only_flatpak).unwrap();
        assert_eq!(exe, "flatpak");
        assert!(args.starts_with("run com.heroicgameslauncher.hgl --no-gui"));
    }

    #[test]
    fn reads_heroic_epic_gog_and_sideload() {
        let d = tempfile::tempdir().unwrap();
        let h = d.path();
        put(h, ".config/heroic/legendaryConfig/legendary/installed.json", r#"{"Sugar":{"title":"Celeste","install_path":"/g/celeste","executable":"Celeste.exe","install_size":42}}"#);
        put(h, ".config/heroic/gog_store/installed.json", r#"{"installed":[{"appName":"123","install_path":"/g/w3"},{"nome":"sem appName"}]}"#);
        put(h, ".config/heroic/store_cache/gog_library.json", r#"{"games":[{"app_name":"123","title":"The Witcher 3"}]}"#);
        put(h, ".config/heroic/sideload_apps/library.json", r#"{"games":[{"app_name":"x1","title":"Meu Jogo","folder_name":"/g/x1","install":{"executable":"/g/x1/run"}}]}"#);
        let g = heroic_games(h);
        assert_eq!(g.len(), 3);
        assert!(g.iter().any(|g| g.id == "epic:Sugar" && g.name == "Celeste" && g.size == Some(42)));
        assert!(g.iter().any(|g| g.id == "gog:123" && g.name == "The Witcher 3"));
        assert!(g.iter().any(|g| g.id == "heroic:x1" && g.exe.as_deref() == Some("/g/x1/run")));
    }

    #[test]
    fn broken_json_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), ".config/heroic/legendaryConfig/legendary/installed.json", "{ quebrado");
        assert!(heroic_games(d.path()).is_empty());
    }

    #[test]
    fn reads_lutris_yml_basics() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), ".local/share/lutris/games/celeste-1.yml", "name: 'Celeste'\nrunner: linux\ngame:\n  exe: /g/celeste/Celeste\n  working_dir: /g\nsystem:\n  exe: nao-eh-este\n");
        put(d.path(), ".local/share/lutris/games/sem-nome.yml", "runner: wine\n");
        let g = lutris_games(d.path());
        assert_eq!(g.len(), 2);
        let c = g.iter().find(|g| g.id == "lutris:celeste-1").unwrap();
        assert_eq!((c.name.as_str(), c.exe.as_deref(), c.install_dir.as_deref()), ("Celeste", Some("/g/celeste/Celeste"), Some("/g/celeste")));
        assert_eq!(g.iter().find(|g| g.id == "lutris:sem-nome").unwrap().name, "sem-nome");
    }

    #[test]
    fn detect_all_merges_sorts_and_includes_custom() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), ".local/share/Steam/steamapps/appmanifest_1.acf", &acf(1, "zebra", "z"));
        let all = detect_all(d.path(), &[CustomGame { name: "Alfa".into(), exe: "/x/alfa".into(), args: String::new() }]);
        assert_eq!(all.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["Alfa", "zebra"]);
        assert_eq!(all[0].source, Source::Custom);
    }
}
