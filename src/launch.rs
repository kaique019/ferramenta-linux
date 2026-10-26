//! Perfis de jogo: transforma o perfil (GameMode, MangoHud, GPU dedicada, prioridade, resolução, variáveis...) em um
//! comando e, para jogos fora da Steam, executa com programas auxiliares e governador "performance" restaurável.
//!
//! Nada passa por shell: argumentos e variáveis viram listas, então aspas e `;` digitados pelo usuário nunca executam.

use std::ffi::CString;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::gpu::GpuVendor;
use crate::power;
use crate::settings::{GameProfile, HelperProgram};
use crate::util::{Cancel, Roots, User, have, shell_quote};

/// Separa uma linha de argumentos respeitando aspas simples/duplas e barra invertida (sem expandir nada).
pub fn parse_args(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut has) = (vec![], String::new(), false);
    let mut it = s.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = it.next() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (Some(q), c) if c == q => quote = None,
            // fora de aspas a barra escapa qualquer caractere; entre aspas duplas, só $ ` " \\ (como no shell),
            // então "C:\Games\x" continua sendo um caminho do Wine
            (None, '\\') => match it.next() {
                Some(n) => {
                    cur.push(n);
                    has = true;
                }
                None => return Err("barra invertida no fim".into()),
            },
            (Some('"'), '\\') => match it.next() {
                Some(n @ ('$' | '`' | '"' | '\\')) => cur.push(n),
                Some('\n') => {}
                Some(n) => {
                    cur.push('\\');
                    cur.push(n);
                }
                None => return Err("barra invertida no fim".into()),
            },
            (_, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if quote.is_some() {
        return Err("aspas sem fechar".into());
    }
    if has {
        out.push(cur);
    }
    Ok(out)
}

/// Linhas `CHAVE=valor` (vazias e `#` são ignoradas). A chave precisa ser um nome de variável válido.
pub fn parse_env(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut v = vec![];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (k, val) = line.split_once('=').ok_or_else(|| format!("linha sem '=': {line:?}"))?;
        let k = k.trim();
        let ok = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(format!("nome de variável inválido: {k:?}"));
        }
        v.push((k.to_string(), val.trim().to_string()));
    }
    Ok(v)
}

/// Argumentos de resolução/janela/taxa para cada família de engine.
pub fn display_args(style: &str, res: &str, win_mode: &str, hz: u32) -> Result<Vec<String>, String> {
    let size = if res == "default" {
        None
    } else {
        let (w, h) = res.split_once('x').ok_or_else(|| format!("resolução inválida: {res}"))?;
        let (w, h): (u32, u32) = (w.parse().map_err(|_| format!("resolução inválida: {res}"))?, h.parse().map_err(|_| format!("resolução inválida: {res}"))?);
        if !(320..=15360).contains(&w) || !(200..=8640).contains(&h) {
            return Err(format!("resolução fora do limite: {res}"));
        }
        Some((w, h))
    };
    let s = |x: &str| x.to_string();
    let mut a = vec![];
    match style {
        "source" => {
            if let Some((w, h)) = size {
                a.extend([s("-w"), w.to_string(), s("-h"), h.to_string()]);
            }
            match win_mode {
                "full" => a.push(s("-fullscreen")),
                "borderless" => a.extend([s("-windowed"), s("-noborder")]),
                "window" => a.push(s("-windowed")),
                _ => {}
            }
            if hz > 0 {
                a.extend([s("-freq"), hz.to_string()]);
            }
        }
        "unity" => {
            if let Some((w, h)) = size {
                a.extend([s("-screen-width"), w.to_string(), s("-screen-height"), h.to_string()]);
            }
            match win_mode {
                "full" => a.extend([s("-screen-fullscreen"), s("1")]),
                "borderless" => a.push(s("-popupwindow")),
                "window" => a.extend([s("-screen-fullscreen"), s("0")]),
                _ => {}
            }
        }
        "unreal" => {
            if let Some((w, h)) = size {
                a.extend([format!("-ResX={w}"), format!("-ResY={h}")]);
            }
            match win_mode {
                "full" => a.push(s("-fullscreen")),
                "borderless" | "window" => a.push(s("-windowed")),
                _ => {}
            }
        }
        other => return Err(format!("estilo de engine desconhecido: {other}")),
    }
    Ok(a)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plan {
    pub env: Vec<(String, String)>,
    /// Programas que envolvem o jogo, de fora para dentro (ex.: gamemoderun, mangohud).
    pub wrappers: Vec<String>,
    /// Prioridade (valor de nice) aplicada pelo processo pai, que roda como root; só vale quando há root.
    pub nice: Option<i32>,
    pub args: Vec<String>,
    pub warnings: Vec<String>,
}

/// Monta o plano. `dgpu` diz o fabricante da GPU dedicada (para o PRIME); `available` informa quais programas existem.
pub fn plan(p: &GameProfile, dgpu: Option<GpuVendor>, is_root: bool, available: &dyn Fn(&str) -> bool) -> Result<Plan, String> {
    let mut pl = Plan { env: parse_env(&p.env)?, args: display_args(&p.style, &p.res, &p.win_mode, p.hz)?, ..Default::default() };
    pl.args.extend(parse_args(&p.extra)?);
    let nice = match p.priority.as_str() {
        "normal" => None,
        "above" => Some(-5),
        "high" => Some(-10),
        o => return Err(format!("prioridade desconhecida: {o}")),
    };
    if let Some(n) = nice {
        if is_root {
            pl.nice = Some(n);
        } else {
            pl.warnings.push("prioridade maior que a normal exige root; ignorada".into());
        }
    }
    if p.gamemode {
        if available("gamemoderun") { pl.wrappers.push("gamemoderun".into()) } else { pl.warnings.push("gamemoderun não encontrado (instale o gamemode)".into()) }
    }
    if p.mangohud {
        if available("mangohud") { pl.wrappers.push("mangohud".into()) } else { pl.warnings.push("mangohud não encontrado (instale o mangohud)".into()) }
    }
    if p.dgpu {
        match dgpu {
            Some(GpuVendor::Nvidia) => pl.env.extend([("__NV_PRIME_RENDER_OFFLOAD".to_string(), "1".to_string()), ("__GLX_VENDOR_LIBRARY_NAME".into(), "nvidia".into()), ("__VK_LAYER_NV_optimus".into(), "NVIDIA_only".into())]),
            Some(_) => pl.env.push(("DRI_PRIME".into(), "1".into())),
            None => pl.warnings.push("nenhuma GPU dedicada identificada; opção ignorada".into()),
        }
    }
    Ok(pl)
}

/// Texto para colar em "Opções de inicialização" da Steam.
pub fn steam_options(pl: &Plan) -> String {
    let mut parts: Vec<String> = pl.env.iter().map(|(k, v)| format!("{k}={}", shell_quote(v))).collect();
    parts.extend(pl.wrappers.iter().map(|w| shell_quote(w)));
    parts.push("%command%".into());
    parts.extend(pl.args.iter().map(|a| shell_quote(a)));
    parts.join(" ")
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub code: Option<i32>,
    pub cancelled: bool,
    pub warnings: Vec<String>,
}

/// Usuário em nome de quem o jogo roda quando o programa foi aberto com sudo (nunca como root).
#[derive(Clone, Debug)]
pub struct RunAs {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: std::path::PathBuf,
}

impl RunAs {
    /// `Some` quando somos root em nome de outro usuário.
    pub fn current() -> Option<RunAs> {
        (crate::util::is_root() && User::is_switched()).then(|| {
            let u = User::target();
            RunAs { name: u.name.clone(), uid: u.uid, gid: u.gid, home: u.home.clone() }
        })
    }
}

fn groups_for(name: &str, gid: u32) -> Vec<libc::gid_t> {
    let Ok(c) = CString::new(name) else { return vec![gid] };
    let mut n: libc::c_int = 64;
    let mut buf: Vec<libc::gid_t> = vec![0; 64];
    for _ in 0..4 {
        // SAFETY: `buf` tem `n` posições; getgrouplist atualiza `n` com o total necessário.
        let r = unsafe { libc::getgrouplist(c.as_ptr(), gid, buf.as_mut_ptr(), &mut n) };
        if r >= 0 {
            buf.truncate(n.max(0) as usize);
            return buf;
        }
        buf.resize(n.max(1) as usize, 0);
    }
    vec![gid]
}

/// Prepara o filho: grupo de processos próprio (para encerrar a árvore inteira) e, sob sudo, queda de privilégios
/// para o usuário (grupos, gid e uid; falhar em qualquer passo impede o início) com o ambiente dele.
fn prepare(cmd: &mut Command, run_as: Option<&RunAs>) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let Some(u) = run_as else { return };
    cmd.env("HOME", &u.home).env("USER", &u.name).env("LOGNAME", &u.name);
    let run = std::path::PathBuf::from(format!("/run/user/{}", u.uid));
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() && run.is_dir() {
        cmd.env("XDG_RUNTIME_DIR", &run);
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            cmd.env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}/bus", run.display()));
        }
    }
    let (uid, gid, groups) = (u.uid, u.gid, groups_for(&u.name, u.gid));
    // SAFETY: só chamadas de sistema simples (setgroups/setgid/setuid), seguras entre fork e exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0 || libc::setgid(gid) != 0 || libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn set_nice(child: &Child, nice: Option<i32>) {
    if let Some(n) = nice {
        // SAFETY: setpriority só altera a prioridade do processo `pid`.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, child.id(), n);
        }
    }
}

/// Encerra o grupo de processos do filho: SIGTERM, espera até `grace` e então SIGKILL. Sempre colhe o filho.
fn kill_group(child: &mut Child, grace: Duration) {
    let pgid = child.id() as i32;
    // SAFETY: kill com pid negativo atinge o grupo que criamos com process_group(0).
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let end = Instant::now() + grace;
    while Instant::now() < end {
        if matches!(child.try_wait(), Ok(Some(_))) {
            // o líder saiu, mas netos podem ter ignorado o SIGTERM
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: idem.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

struct Running {
    helpers: Vec<(Child, bool)>,
    governors: Option<(Roots, std::collections::BTreeMap<String, String>)>,
}

impl Drop for Running {
    fn drop(&mut self) {
        for (c, close) in &mut self.helpers {
            if *close {
                kill_group(c, Duration::from_secs(2));
            } else {
                // não fecha junto: segue vivo, sem bloquear; só colhe se já terminou
                let _ = c.try_wait();
            }
        }
        if let Some((r, snap)) = self.governors.take() {
            power::restore_governors(&r, &snap);
        }
    }
}

fn start_helper(h: &HelperProgram, run_as: Option<&RunAs>) -> Result<Child, String> {
    if h.path.trim().is_empty() {
        return Err("caminho vazio".into());
    }
    let mut cmd = Command::new(&h.path);
    cmd.args(parse_args(&h.args)?).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    prepare(&mut cmd, run_as);
    cmd.spawn().map_err(|e| format!("{}: {e}", h.path))
}

/// Executa o jogo (fora da Steam) com o perfil e espera terminar. Sob sudo, jogo e auxiliares rodam como o usuário
/// (não como root). Auxiliares e governadores são restaurados ao final, mesmo em caso de cancelamento ou erro.
pub fn run(p: &GameProfile, exe: &str, base_args: &str, dgpu: Option<GpuVendor>, cancel: &Cancel) -> Result<Outcome, String> {
    run_as(p, exe, base_args, dgpu, cancel, RunAs::current().as_ref())
}

pub fn run_as(p: &GameProfile, exe: &str, base_args: &str, dgpu: Option<GpuVendor>, cancel: &Cancel, run_as: Option<&RunAs>) -> Result<Outcome, String> {
    let mut pl = plan(p, dgpu, crate::util::is_root(), &have)?;
    let mut run = Running { helpers: vec![], governors: None };
    if p.performance_governor {
        let roots = Roots::system();
        let snap = power::governors(&roots);
        if snap.is_empty() {
            pl.warnings.push("este sistema não expõe o governador de CPU".into());
        } else if !snap.values().all(|g| g == "performance") {
            match power::set_governor(&roots, "performance") {
                Ok(_) => run.governors = Some((roots, snap)),
                Err(e) => pl.warnings.push(format!("não consegui trocar o governador: {e}")),
            }
        }
    }
    for h in &p.helpers {
        match start_helper(h, run_as) {
            Ok(c) => run.helpers.push((c, h.close_with_game)),
            Err(e) => pl.warnings.push(format!("programa auxiliar não iniciou ({e})")),
        }
    }
    let mut argv: Vec<String> = pl.wrappers.clone();
    argv.push(exe.to_string());
    argv.extend(parse_args(base_args)?);
    argv.extend(pl.args.clone());
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).envs(pl.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null());
    prepare(&mut cmd, run_as);
    let mut child = cmd.spawn().map_err(|e| format!("não consegui iniciar {exe}: {e}"))?;
    set_nice(&child, pl.nice);
    let mut cancelled = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                // o líder terminou; remanescentes do mesmo grupo (ex.: launcher que deixa filhos) são encerrados com ele
                // apenas quando o jogo foi cancelado; no término normal deixamos como estão
                break st.code();
            }
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child, Duration::from_secs(2));
                return Err(format!("erro ao esperar o jogo: {e}"));
            }
        }
        if cancel.is_cancelled() {
            cancelled = true;
            kill_group(&mut child, Duration::from_secs(3));
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    drop(run);
    Ok(Outcome { code, cancelled, warnings: pl.warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(_: &str) -> bool {
        true
    }
    fn no(_: &str) -> bool {
        false
    }

    #[test]
    fn splits_args_with_quotes_and_never_expands() {
        assert_eq!(parse_args(r#"-a "b c" 'd e' f\ g $HOME `x` ;ls"#).unwrap(), ["-a", "b c", "d e", "f g", "$HOME", "`x`", ";ls"]);
        assert_eq!(parse_args("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_args(r#""" x"#).unwrap(), ["", "x"]);
        assert!(parse_args("'aberta").is_err() && parse_args("x\\").is_err());
    }

    #[test]
    fn double_quotes_keep_wine_paths_but_still_escape_quotes_and_dollars() {
        assert_eq!(parse_args(r#"-w "C:\Games\x" "a \"b\" \$HOME" 'm\n'"#).unwrap(), ["-w", r"C:\Games\x", r#"a "b" $HOME"#, r"m\n"]);
    }

    #[test]
    fn parses_env_lines_and_rejects_bad_names() {
        assert_eq!(parse_env("# c\nDXVK_HUD=fps\n\n  PROTON_NO_ESYNC = 1 \nA=b=c").unwrap(), [("DXVK_HUD".into(), "fps".into()), ("PROTON_NO_ESYNC".into(), "1".into()), ("A".into(), "b=c".into())]);
        for bad in ["SEM_IGUAL", "1A=x", "A B=x", "=x", "A;rm=x"] {
            assert!(parse_env(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn display_args_per_engine() {
        assert_eq!(display_args("source", "1920x1080", "borderless", 144).unwrap(), ["-w", "1920", "-h", "1080", "-windowed", "-noborder", "-freq", "144"]);
        assert_eq!(display_args("unity", "2560x1440", "full", 0).unwrap(), ["-screen-width", "2560", "-screen-height", "1440", "-screen-fullscreen", "1"]);
        assert_eq!(display_args("unreal", "1280x720", "window", 0).unwrap(), ["-ResX=1280", "-ResY=720", "-windowed"]);
        assert!(display_args("source", "default", "default", 0).unwrap().is_empty());
        for bad in [("source", "1x1"), ("source", "axb"), ("source", "99999x99999"), ("godot", "default")] {
            assert!(display_args(bad.0, bad.1, "default", 0).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn plan_orders_wrappers_and_adds_prime_env() {
        let p = GameProfile { priority: "high".into(), gamemode: true, mangohud: true, dgpu: true, env: "FOO=1".into(), extra: "-novid".into(), ..Default::default() };
        let pl = plan(&p, Some(GpuVendor::Nvidia), true, &yes).unwrap();
        assert_eq!(pl.wrappers, ["gamemoderun", "mangohud"]);
        assert_eq!(pl.nice, Some(-10));
        assert!(pl.env.iter().any(|(k, v)| k == "__NV_PRIME_RENDER_OFFLOAD" && v == "1") && pl.env.iter().any(|(k, _)| k == "FOO"));
        assert_eq!(pl.args.last().unwrap(), "-novid");
        let amd = plan(&GameProfile { dgpu: true, ..Default::default() }, Some(GpuVendor::Amd), false, &yes).unwrap();
        assert_eq!(amd.env, [("DRI_PRIME".to_string(), "1".to_string())]);
    }

    #[test]
    fn missing_tools_and_privileges_become_warnings_not_failures() {
        let p = GameProfile { priority: "above".into(), gamemode: true, mangohud: true, dgpu: true, ..Default::default() };
        let pl = plan(&p, None, false, &no).unwrap();
        assert!(pl.wrappers.is_empty());
        assert_eq!(pl.warnings.len(), 4);
        assert!(plan(&GameProfile { priority: "turbo".into(), ..Default::default() }, None, false, &yes).is_err());
        assert!(plan(&GameProfile { env: "ruim".into(), ..Default::default() }, None, false, &yes).is_err());
    }

    #[test]
    fn steam_options_are_quoted_and_contain_command_placeholder() {
        let p = GameProfile { gamemode: true, mangohud: true, env: "MSG=oi mundo".into(), extra: "-console \"a b\"".into(), ..Default::default() };
        let o = steam_options(&plan(&p, None, false, &yes).unwrap());
        assert!(o.contains("%command%") && o.contains("gamemoderun mangohud %command%"));
        assert!(o.starts_with("MSG='oi mundo'"), "{o}");
        assert!(o.ends_with("-console 'a b'"), "{o}");
    }

    #[test]
    fn run_returns_exit_code_and_passes_env_and_args() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let p = GameProfile { env: "MEU_VALOR=ok".into(), extra: format!("-c 'echo $MEU_VALOR $0 > {}; exit 3' arg0", out.display()), style: "source".into(), ..Default::default() };
        let o = run(&p, "/bin/sh", "", None, &Cancel::new()).unwrap();
        assert_eq!(o.code, Some(3));
        assert_eq!(std::fs::read_to_string(out).unwrap().trim(), "ok arg0");
        assert!(run(&p, "/nao/existe", "", None, &Cancel::new()).is_err());
    }

    #[test]
    fn helper_closes_with_game_and_cancel_kills_the_game() {
        let d = tempfile::tempdir().unwrap();
        let pidf = d.path().join("pid");
        let helper = HelperProgram { path: "/bin/sh".into(), args: format!("-c 'echo $$ > {}; sleep 30'", pidf.display()), close_with_game: true };
        let p = GameProfile { helpers: vec![helper], extra: "-c 'sleep 0.5'".into(), ..Default::default() };
        let t = std::time::Instant::now();
        let o = run(&p, "/bin/sh", "", None, &Cancel::new()).unwrap();
        assert_eq!(o.code, Some(0));
        let pid: u32 = std::fs::read_to_string(&pidf).unwrap().trim().parse().unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "auxiliar ainda vivo");
        assert!(t.elapsed() < Duration::from_secs(10));
        let c = Cancel::new();
        let c2 = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.cancel();
        });
        let long = GameProfile { extra: "-c 'sleep 30'".into(), ..Default::default() };
        let o = run(&long, "/bin/sh", "", None, &c).unwrap();
        assert!(o.cancelled && o.code.is_none());
    }

    #[test]
    fn broken_helper_only_warns() {
        let p = GameProfile { helpers: vec![HelperProgram { path: "/nao/existe".into(), args: String::new(), close_with_game: true }], extra: "-c true".into(), ..Default::default() };
        let o = run(&p, "/bin/sh", "", None, &Cancel::new()).unwrap();
        assert_eq!(o.code, Some(0));
        assert_eq!(o.warnings.len(), 1);
    }

    fn alive(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(st) => st.rsplit(')').next().map(|r| !r.trim_start().starts_with('Z')).unwrap_or(false),
            Err(_) => false,
        }
    }

    fn pid_from(path: &std::path::Path) -> u32 {
        for _ in 0..100 {
            if let Some(n) = std::fs::read_to_string(path).ok().and_then(|t| t.trim().parse().ok()) {
                return n;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("o pid não apareceu em {path:?}");
    }

    fn open_dir(d: &tempfile::TempDir) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    }

    #[test]
    fn under_sudo_the_game_runs_as_the_user_with_groups_home_and_priority() {
        if !crate::util::is_root() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        open_dir(&d);
        let out = d.path().join("out");
        let p = GameProfile { priority: "high".into(), extra: format!("-c 'sleep 0.4; echo uid=$(id -u) gid=$(id -g) home=$HOME user=$USER nice=$(nice) > {}' x", out.display()), ..Default::default() };
        let who = RunAs { name: "nobody".into(), uid: 65534, gid: 65534, home: "/nonexistent".into() };
        let o = run_as(&p, "/bin/sh", "", None, &Cancel::new(), Some(&who)).unwrap();
        assert_eq!(o.code, Some(0), "{:?}", o.warnings);
        let t = std::fs::read_to_string(out).unwrap();
        assert!(t.contains("uid=65534") && t.contains("gid=65534") && t.contains("home=/nonexistent") && t.contains("user=nobody"), "{t}");
        assert!(t.contains("nice=-10"), "a prioridade é aplicada pelo pai (root) antes de o filho trabalhar: {t}");
    }

    #[test]
    fn user_data_in_settings_cannot_gain_root_through_the_game_or_helpers() {
        if !crate::util::is_root() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        open_dir(&d);
        let (g, h) = (d.path().join("game"), d.path().join("helper"));
        let helper = HelperProgram { path: "/bin/sh".into(), args: format!("-c 'id -u > {}; sleep 5'", h.display()), close_with_game: true };
        let p = GameProfile { helpers: vec![helper], extra: format!("-c 'sleep 0.4; id -u > {}' x", g.display()), ..Default::default() };
        let who = RunAs { name: "nobody".into(), uid: 65534, gid: 65534, home: "/nonexistent".into() };
        run_as(&p, "/bin/sh", "", None, &Cancel::new(), Some(&who)).unwrap();
        assert_eq!(std::fs::read_to_string(g).unwrap().trim(), "65534");
        assert_eq!(std::fs::read_to_string(h).unwrap().trim(), "65534", "auxiliar também não roda como root");
    }

    #[test]
    fn cancel_kills_the_whole_process_tree_of_the_game() {
        let d = tempfile::tempdir().unwrap();
        let pidf = d.path().join("child");
        let p = GameProfile { extra: format!("-c 'sleep 30 & echo $! > {}; wait' x", pidf.display()), ..Default::default() };
        let c = Cancel::new();
        let c2 = c.clone();
        let pidf2 = pidf.clone();
        let h = std::thread::spawn(move || {
            let _ = pid_from(&pidf2);
            c2.cancel();
        });
        let t = Instant::now();
        let o = run(&p, "/bin/sh", "", None, &c).unwrap();
        h.join().unwrap();
        assert!(o.cancelled && t.elapsed() < Duration::from_secs(10));
        let child = pid_from(&pidf);
        assert!(!alive(child), "o neto do jogo (sleep) foi encerrado junto");
    }

    #[test]
    fn helper_children_die_with_the_game_but_a_non_closing_helper_neither_blocks_nor_dies() {
        let d = tempfile::tempdir().unwrap();
        let (kid, keep) = (d.path().join("kid"), d.path().join("keep"));
        let closing = HelperProgram { path: "/bin/sh".into(), args: format!("-c 'sleep 30 & echo $! > {}; wait'", kid.display()), close_with_game: true };
        let staying = HelperProgram { path: "/bin/sh".into(), args: format!("-c 'echo $$ > {}; sleep 30'", keep.display()), close_with_game: false };
        let p = GameProfile { helpers: vec![closing, staying], extra: "-c 'sleep 0.6' x".into(), ..Default::default() };
        let t = Instant::now();
        let o = run(&p, "/bin/sh", "", None, &Cancel::new()).unwrap();
        assert_eq!(o.code, Some(0));
        assert!(t.elapsed() < Duration::from_secs(8), "run() não pode ficar preso esperando o auxiliar que não fecha: {:?}", t.elapsed());
        assert!(!alive(pid_from(&kid)), "filho do auxiliar que fecha junto foi encerrado");
        let k = pid_from(&keep);
        assert!(alive(k), "auxiliar que não fecha junto continua vivo");
        // limpeza do teste
        unsafe {
            libc::kill(-(k as i32), libc::SIGKILL);
            libc::kill(k as i32, libc::SIGKILL);
        }
    }
}
