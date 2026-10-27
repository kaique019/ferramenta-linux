//! Utilidades compartilhadas: leitura de /proc e /sys, execução de comandos externos
//! com tempo limite, caminhos XDG, formatação e tarefas em segundo plano.

use std::cmp::Ordering;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------------------------
// Raízes dos sistemas de arquivos virtuais
// ---------------------------------------------------------------------------------------------

/// Raízes de /sys, /proc, /etc e /dev. Nos testes apontam para uma árvore falsa em um diretório temporário.
#[derive(Clone, Debug)]
pub struct Roots {
    pub sys: PathBuf,
    pub proc: PathBuf,
    pub etc: PathBuf,
    pub dev: PathBuf,
}

impl Roots {
    /// Os diretórios reais do sistema.
    pub fn system() -> Self {
        Roots { sys: "/sys".into(), proc: "/proc".into(), etc: "/etc".into(), dev: "/dev".into() }
    }

    /// Uma árvore falsa dentro de `base` (usada nos testes).
    pub fn under(base: &Path) -> Self {
        Roots { sys: base.join("sys"), proc: base.join("proc"), etc: base.join("etc"), dev: base.join("dev") }
    }
}

impl Default for Roots {
    fn default() -> Self {
        Self::system()
    }
}

// ---------------------------------------------------------------------------------------------
// Leitura de arquivos
// ---------------------------------------------------------------------------------------------

/// Lê um arquivo de texto e remove espaços das pontas. `None` se não existir, não puder ser lido ou estiver vazio.
pub fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Lê um arquivo com um único número (comum em /sys).
pub fn read_num<T: FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read_trim(path)?.parse().ok()
}

/// Linhas de um arquivo de texto (vazio se não puder ser lido).
pub fn read_lines(path: impl AsRef<Path>) -> Vec<String> {
    fs::read_to_string(path).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

/// Comparação "natural": `hwmon2` vem antes de `hwmon10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(&c) = ai.peek() {
                    if c.is_ascii_digit() {
                        na.push(c);
                        ai.next();
                    } else {
                        break;
                    }
                }
                let mut nb = String::new();
                while let Some(&c) = bi.peek() {
                    if c.is_ascii_digit() {
                        nb.push(c);
                        bi.next();
                    } else {
                        break;
                    }
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Nomes das entradas de um diretório em ordem natural (vazio se não existir).
pub fn dir_names(path: impl AsRef<Path>) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(path)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| natural_cmp(a, b));
    v
}

/// Lê uma lista no formato "chave: valor" (como /proc/meminfo e /proc/cpuinfo), mantendo a ordem.
pub fn parse_colon_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Lê um arquivo no formato `CHAVE=valor` ou `CHAVE="valor"` (como /etc/os-release).
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Comandos externos
// ---------------------------------------------------------------------------------------------

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Procura um programa no PATH e nos diretórios sbin (que muitas distros não incluem no PATH de usuário comum).
pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return is_executable(&p).then_some(p);
    }
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for extra in ["/usr/local/bin", "/usr/bin", "/bin", "/usr/local/sbin", "/usr/sbin", "/sbin"] {
        let e = PathBuf::from(extra);
        if !dirs.contains(&e) {
            dirs.push(e);
        }
    }
    dirs.into_iter().map(|d| d.join(cmd)).find(|p| is_executable(p))
}

/// O programa existe?
pub fn have(cmd: &str) -> bool {
    which(cmd).is_some()
}

/// Resultado de um comando externo.
#[derive(Clone, Debug, Default)]
pub struct CmdOut {
    /// Código de saída (`None` se foi encerrado por sinal ou por tempo).
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CmdOut {
    pub fn ok(&self) -> bool {
        self.status == Some(0)
    }

    /// stdout + stderr, útil para mostrar o motivo de uma falha.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// Última linha não vazia da saída combinada.
    pub fn last_line(&self) -> String {
        self.combined().lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
    }
}

/// Executa um comando com tempo limite. `LC_ALL=C` garante saída estável para o parser.
/// Devolve `None` se o programa não existe ou não pôde ser iniciado.
pub fn run(cmd: &str, args: &[&str], timeout: Duration) -> Option<CmdOut> {
    run_env(cmd, args, timeout, &[])
}

/// Como [`run`], com variáveis de ambiente extras.
pub fn run_env(cmd: &str, args: &[&str], timeout: Duration, envs: &[(&str, &str)]) -> Option<CmdOut> {
    let path = which(cmd)?;
    let mut c = Command::new(path);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("LC_ALL", "C");
    for (k, v) in envs {
        c.env(k, v);
    }
    let mut child = c.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let t_out = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(Duration::from_millis(4));
            }
            Err(_) => break None,
        }
    };
    // Em caso de tempo esgotado, processos netos podem manter o pipe aberto: não esperamos por eles.
    let (so, se) = if timed_out {
        (Vec::new(), Vec::new())
    } else {
        (t_out.join().unwrap_or_default(), t_err.join().unwrap_or_default())
    };
    Some(CmdOut {
        status,
        stdout: String::from_utf8_lossy(&so).into_owned(),
        stderr: String::from_utf8_lossy(&se).into_owned(),
        timed_out,
    })
}

/// Executa e devolve o stdout somente se o comando terminou com sucesso.
pub fn run_stdout(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let o = run(cmd, args, timeout)?;
    o.ok().then_some(o.stdout)
}

// ---------------------------------------------------------------------------------------------
// Usuário, privilégios e diretórios
// ---------------------------------------------------------------------------------------------

/// O processo está rodando como root?
pub fn is_root() -> bool {
    // SAFETY: geteuid não tem pré-condições.
    unsafe { libc::geteuid() == 0 }
}

/// O usuário "dono" da sessão: o próprio, ou quem chamou `sudo`/`pkexec` quando o programa roda como root.
#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

fn passwd_lookup(passwd_text: &str, uid: u32) -> Option<User> {
    passwd_text.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() >= 6 && f[2].parse::<u32>().ok()? == uid {
            Some(User { name: f[0].to_string(), uid, gid: f[3].parse().ok()?, home: PathBuf::from(f[5]) })
        } else {
            None
        }
    })
}

impl User {
    /// Usuário alvo (calculado uma vez). Sob sudo, as configurações, o autostart, a Steam etc. são os de quem chamou o sudo.
    pub fn target() -> &'static User {
        static T: OnceLock<User> = OnceLock::new();
        T.get_or_init(|| {
            // SAFETY: geteuid/getegid não têm pré-condições.
            let (euid, egid) = unsafe { (libc::geteuid(), libc::getegid()) };
            let mut uid = euid;
            if euid == 0 {
                let from_env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
                if let Some(u) = from_env("SUDO_UID").or_else(|| from_env("PKEXEC_UID")) {
                    uid = u;
                }
            }
            let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
            if let Some(u) = passwd_lookup(&passwd, uid) {
                return u;
            }
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/root"));
            User {
                name: std::env::var("USER").unwrap_or_else(|_| "root".into()),
                uid,
                gid: if uid == euid { egid } else { uid },
                home,
            }
        })
    }

    /// O alvo é diferente do usuário efetivo (programa rodando com sudo)?
    pub fn is_switched() -> bool {
        // SAFETY: geteuid não tem pré-condições.
        Self::target().uid != unsafe { libc::geteuid() }
    }
}

pub fn home_dir() -> PathBuf {
    User::target().home.clone()
}

fn xdg_dir(var: &str, default_rel: &str) -> PathBuf {
    if !User::is_switched()
        && let Some(p) = std::env::var_os(var).map(PathBuf::from)
            && p.is_absolute() {
                return p;
            }
    home_dir().join(default_rel)
}

/// `$XDG_CONFIG_HOME` ou `~/.config`.
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME` ou `~/.local/share`.
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_STATE_HOME` ou `~/.local/state`.
pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state")
}

/// Pasta de configurações do programa.
pub fn app_config_dir() -> PathBuf {
    config_dir().join("pingu")
}

/// Pasta de estado do programa (backups, histórico).
pub fn app_state_dir() -> PathBuf {
    state_dir().join("pingu")
}

thread_local! {
    static FS_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Faz a E/S de arquivos desta thread com a identidade do usuário-alvo (`setfsuid`, que vale só para a thread).
///
/// Sob `sudo` o programa é root, mas lê e grava em pastas que o usuário controla. Se fizesse isso como root, um
/// symlink plantado pelo usuário (`settings.json.tmp1234 -> /etc/shadow`) viraria escrita ou leitura arbitrária como
/// root. Com o fsuid do usuário, o kernel recusa tudo o que o próprio usuário não poderia fazer, e os arquivos
/// criados já saem com o dono certo (sem `chown`). Quando não há troca de usuário, não faz nada.
pub struct FsUser {
    active: bool,
}

impl FsUser {
    pub fn enter() -> Result<FsUser, String> {
        if is_root() && User::is_switched() {
            let u = User::target();
            Self::enter_as(u.uid, u.gid)
        } else {
            Ok(FsUser { active: false })
        }
    }

    pub fn enter_as(uid: u32, gid: u32) -> Result<FsUser, String> {
        let first = FS_DEPTH.with(|d| {
            d.set(d.get() + 1);
            d.get() == 1
        });
        let guard = FsUser { active: true };
        if first {
            // SAFETY: setfsuid/setfsgid só trocam a identidade de arquivos da thread atual.
            unsafe {
                libc::setfsgid(gid);
                libc::setfsuid(uid);
                // chamar com -1 devolve o valor atual: confirma que a troca valeu (se não, nada é feito)
                if libc::setfsuid(u32::MAX) as u32 != uid {
                    return Err("não consegui assumir a identidade do usuário para mexer nos arquivos dele".into());
                }
            }
        }
        Ok(guard)
    }
}

impl Drop for FsUser {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let last = FS_DEPTH.with(|d| {
            d.set(d.get() - 1);
            d.get() == 0
        });
        if last {
            // SAFETY: devolve a identidade de arquivos da thread (root) ao valor original.
            unsafe {
                libc::setfsuid(0);
                libc::setfsgid(0);
            }
        }
    }
}

/// Executa `f` com a identidade de arquivos do usuário-alvo (veja [`FsUser`]).
pub fn as_user<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    let _g = FsUser::enter()?;
    Ok(f())
}

/// Como [`as_user`] para funções que já devolvem `io::Result`.
pub fn as_user_io<R>(f: impl FnOnce() -> std::io::Result<R>) -> std::io::Result<R> {
    as_user(f).map_err(std::io::Error::other)?
}

/// Lê um arquivo de texto de tamanho limitado e só se for arquivo comum (recusa `/dev/zero`, FIFOs e afins que um
/// symlink poderia apontar). `None` se não existir, for grande demais ou não for UTF-8.
pub fn read_small(path: impl AsRef<Path>, max_bytes: u64) -> Option<String> {
    use std::io::Read;
    let f = fs::File::open(path.as_ref()).ok()?;
    let md = f.metadata().ok()?;
    if !md.is_file() || md.len() > max_bytes {
        return None;
    }
    let mut s = String::new();
    f.take(max_bytes + 1).read_to_string(&mut s).ok()?;
    (s.len() as u64 <= max_bytes).then_some(s)
}

/// Cria um diretório (e os pais) como o usuário-alvo, quando roda sob sudo.
pub fn create_dir_for_user(dir: &Path) -> std::io::Result<()> {
    as_user_io(|| fs::create_dir_all(dir))
}

/// Grava um arquivo de forma atômica (temporário + rename), com a posse do usuário-alvo.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    write_atomic_mode(path, contents, 0o644)
}

/// Como [`write_atomic`], com permissões explícitas (ex.: `0o600` para arquivos com chaves de API).
///
/// O temporário tem nome imprevisível e é criado com `O_EXCL|O_NOFOLLOW`; o `rename` troca o próprio caminho final
/// (se for um symlink, o symlink é substituído, nunca seguido).
pub fn write_atomic_mode(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    static N: AtomicU32 = AtomicU32::new(0);
    as_user_io(|| {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        for _ in 0..32 {
            let mut name = path.as_os_str().to_owned();
            name.push(format!(".tmp{}-{}-{:x}", std::process::id(), N.fetch_add(1, Relaxed), nanos ^ N.load(Relaxed).wrapping_mul(0x9E37_79B1)));
            let tmp = PathBuf::from(name);
            let opened = fs::OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&tmp);
            let mut f = match opened {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            let res = f.write_all(contents).and_then(|_| f.sync_all());
            drop(f);
            if let Err(e) = res.and_then(|_| fs::rename(&tmp, path)) {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
            return Ok(());
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "não consegui criar um arquivo temporário único"))
    })
}

/// O usuário atual tem permissão de leitura e escrita no arquivo?
pub fn can_read_write(path: &Path) -> bool {
    match CString::new(path.as_os_str().as_encoded_bytes()) {
        // SAFETY: `c` é uma string C válida pelo tempo da chamada.
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 },
        Err(_) => false,
    }
}

/// Grupos do processo atual (o que vale agora, sem precisar sair e entrar de novo na sessão).
pub fn groups_now() -> Vec<String> {
    run("id", &["-Gn"], Duration::from_secs(2))
        .filter(CmdOut::ok)
        .map(|o| o.stdout.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Grupos do usuário segundo o banco de dados do sistema (inclui os que ainda não valem na sessão atual).
pub fn groups_of_user(user: &str) -> Vec<String> {
    run("id", &["-Gn", user], Duration::from_secs(2))
        .filter(CmdOut::ok)
        .map(|o| o.stdout.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Como executar um comando que precisa de privilégios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// Já é root.
    Root,
    /// `sudo -n` funciona (credenciais em cache).
    SudoCached,
    /// Sem privilégios disponíveis agora.
    None,
}

/// Verifica como é possível obter privilégios neste momento (não pede senha).
pub fn privilege() -> Privilege {
    if is_root() {
        return Privilege::Root;
    }
    match run("sudo", &["-n", "true"], Duration::from_secs(3)) {
        Some(o) if o.ok() => Privilege::SudoCached,
        _ => Privilege::None,
    }
}

/// Executa um comando com privilégios, sem interação. `None` se não houver como elevar.
pub fn run_priv(cmd: &str, args: &[&str], timeout: Duration) -> Option<CmdOut> {
    match privilege() {
        Privilege::Root => run(cmd, args, timeout),
        Privilege::SudoCached => {
            let path = which(cmd)?;
            let p = path.to_string_lossy().into_owned();
            let mut a: Vec<&str> = vec!["-n", &p];
            a.extend_from_slice(args);
            run("sudo", &a, timeout)
        }
        Privilege::None => None,
    }
}

/// Tenta o comando sem privilégios e, se falhar por permissão, tenta com privilégios (quando disponíveis).
pub fn run_maybe_priv(cmd: &str, args: &[&str], timeout: Duration) -> Option<CmdOut> {
    let first = run(cmd, args, timeout)?;
    if first.ok() {
        return Some(first);
    }
    let text = first.combined().to_lowercase();
    let denied = text.contains("permission denied") || text.contains("operation not permitted") || text.contains("must be root") || text.contains("root privileges");
    if denied { run_priv(cmd, args, timeout).or(Some(first)) } else { Some(first) }
}

/// Grava um valor em um arquivo de /sys ou /proc/sys; usa privilégios se o usuário comum não puder escrever.
pub fn write_priv(path: &Path, value: &str) -> Result<(), String> {
    if fs::write(path, value).is_ok() {
        return Ok(());
    }
    let p = path.to_string_lossy().into_owned();
    let o = run_priv("sh", &["-c", "printf '%s' \"$1\" > \"$2\"", "_", value, &p], Duration::from_secs(5))
        .ok_or_else(|| "precisa de privilégios de administrador (rode com sudo)".to_string())?;
    if o.ok() { Ok(()) } else { Err(o.last_line()) }
}

// ---------------------------------------------------------------------------------------------
// Formatação
// ---------------------------------------------------------------------------------------------

/// Bytes em unidades binárias (KiB, MiB, GiB...).
pub fn fmt_bytes(b: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 5 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Bytes em unidades decimais (kB, MB, GB), como os fabricantes de discos anunciam.
pub fn fmt_bytes_si(b: u64) -> String {
    const U: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < 5 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else if v >= 100.0 { format!("{v:.0} {}", U[i]) } else { format!("{v:.1} {}", U[i]) }
}

/// Velocidade de transferência (bytes por segundo).
pub fn fmt_rate(bytes_per_s: f64) -> String {
    format!("{}/s", fmt_bytes(bytes_per_s.max(0.0) as u64))
}

/// Duração em português: "2 d 3 h", "4 min 10 s", "45 s".
pub fn fmt_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    if d > 0 {
        format!("{d} d {h} h {m} min")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min {s} s")
    } else {
        format!("{s} s")
    }
}

/// Frequência em MHz como "3.60 GHz" ou "800 MHz".
pub fn fmt_mhz(mhz: f64) -> String {
    if mhz >= 1000.0 { format!("{:.2} GHz", mhz / 1000.0) } else { format!("{mhz:.0} MHz") }
}

/// Barra de texto com `width` células: `█████░░░░░`.
pub fn text_bar(pct: f64, width: usize) -> String {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width.saturating_sub(filled)))
}

/// Corta o texto em `max` caracteres, com reticências.
pub fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Escapa texto para HTML.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Coloca aspas simples quando o texto tem caracteres especiais para o shell (para exibir comandos).
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Monta o texto de um comando para mostrar ao usuário.
pub fn display_cmd(args: &[String]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------------
// Tempo
// ---------------------------------------------------------------------------------------------

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Formata um instante Unix no fuso local (formato `strftime`).
pub fn format_local(unix: u64, fmt: &str) -> String {
    let Ok(cfmt) = CString::new(fmt) else { return String::new() };
    let t = unix as libc::time_t;
    // SAFETY: `tm` é inicializado por localtime_r; o buffer tem o tamanho informado ao strftime.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0u8; 128];
        let n = libc::strftime(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len(), cfmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

pub fn now_local(fmt: &str) -> String {
    format_local(now_unix(), fmt)
}

// ---------------------------------------------------------------------------------------------
// Tarefas em segundo plano (com progresso e cancelamento)
// ---------------------------------------------------------------------------------------------

/// Sinal de cancelamento compartilhado entre a interface e a tarefa.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::SeqCst);
    }
    /// Ninguém mais além de quem pergunta guarda este sinal (serve para encerrar threads auxiliares).
    pub fn is_orphaned(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::SeqCst)
    }
    /// Espera `d`, acordando antes se for cancelado. Devolve `true` se foi cancelado.
    pub fn sleep(&self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while Instant::now() < end {
            if self.is_cancelled() {
                return true;
            }
            thread::sleep(Duration::from_millis(20).min(end.saturating_duration_since(Instant::now())));
        }
        self.is_cancelled()
    }
}

/// Mensagem de uma tarefa em segundo plano.
#[derive(Debug)]
pub enum TaskMsg<P, R> {
    Progress(P),
    Done(R),
}

/// Tarefa em segundo plano: a interface consulta `poll()` a cada ciclo.
pub struct Task<P, R> {
    rx: Receiver<TaskMsg<P, R>>,
    pub cancel: Cancel,
    finished: bool,
    crashed: bool,
}

impl<P, R> Task<P, R> {
    /// Mensagens que chegaram desde a última consulta.
    pub fn poll(&mut self) -> Vec<TaskMsg<P, R>> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(m) => {
                    if matches!(m, TaskMsg::Done(_)) {
                        self.finished = true;
                    }
                    out.push(m);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.finished {
                        self.finished = true;
                        self.crashed = true;
                    }
                    break;
                }
            }
        }
        out
    }

    /// A tarefa terminou (com resultado ou por falha)?
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// A tarefa terminou sem devolver resultado (falha inesperada)?
    pub fn crashed(&self) -> bool {
        self.crashed
    }
}

/// Inicia `f` em uma thread. `f` recebe uma função para informar progresso e o sinal de cancelamento.
pub fn spawn_task<P, R, F>(f: F) -> Task<P, R>
where
    P: Send + 'static,
    R: Send + 'static,
    F: FnOnce(&dyn Fn(P), &Cancel) -> R + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let cancel = Cancel::new();
    let c2 = cancel.clone();
    thread::spawn(move || {
        let tx_progress = tx.clone();
        let report = move |p: P| {
            let _ = tx_progress.send(TaskMsg::Progress(p));
        };
        let r = f(&report, &c2);
        let _ = tx.send(TaskMsg::Done(r));
    });
    Task { rx, cancel, finished: false, crashed: false }
}

/// Para uso em funções que recebem `&OsStr`.
pub fn os(s: &str) -> &OsStr {
    OsStr::new(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fsuid_of_this_thread() -> u32 {
        let st = fs::read_to_string("/proc/thread-self/status").unwrap();
        let line = st.lines().find(|l| l.starts_with("Uid:")).unwrap();
        line.split_whitespace().nth(4).unwrap().parse().unwrap()
    }

    #[test]
    fn fs_identity_is_per_thread_nested_and_restored() {
        if !is_root() {
            return; // precisa de root para trocar o fsuid
        }
        let before = fsuid_of_this_thread();
        // thread criada ANTES do guard (as novas herdam a identidade de quem as cria): representa a coleta em segundo plano
        let (ask_tx, ask_rx) = mpsc::channel::<()>();
        let (ans_tx, ans_rx) = mpsc::channel::<u32>();
        let bg = thread::spawn(move || {
            while ask_rx.recv().is_ok() {
                ans_tx.send(fsuid_of_this_thread()).unwrap();
            }
        });
        let (inside, other_thread_sees, nested_ok) = {
            let g = FsUser::enter_as(65534, 65534).unwrap();
            let inside = fsuid_of_this_thread();
            ask_tx.send(()).unwrap();
            let other = ans_rx.recv().unwrap();
            let nested = {
                let _g2 = FsUser::enter_as(65534, 65534).unwrap();
                fsuid_of_this_thread() == 65534
            };
            let still = fsuid_of_this_thread() == 65534; // sair do aninhado não solta o externo
            drop(g);
            (inside, other, nested && still)
        };
        assert_eq!(inside, 65534);
        assert_eq!(other_thread_sees, before, "outra thread continua root: a coleta de /sys não perde privilégios");
        assert!(nested_ok);
        assert_eq!(fsuid_of_this_thread(), before, "restaurado ao sair");
        drop(ask_tx);
        bg.join().unwrap();
    }

    #[test]
    fn symlink_to_a_root_only_file_is_unreadable_and_untouchable_as_the_user() {
        if !is_root() {
            return;
        }
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let secret = d.path().join("secret");
        fs::write(&secret, "segredo").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert_eq!(read_small(&link, 100).as_deref(), Some("segredo"), "como root, o symlink leva ao segredo");
        {
            let _g = FsUser::enter_as(65534, 65534).unwrap();
            assert_eq!(read_small(&link, 100), None, "como o usuário, não");
            assert!(fs::OpenOptions::new().write(true).open(&link).is_err());
            // arquivos criados saem com o dono certo, sem chown
            let mine = d.path().join("meu");
            fs::write(&mine, "x").unwrap();
            assert_eq!(fs::metadata(&mine).unwrap().uid(), 65534);
        }
        assert_eq!(fs::read_to_string(&secret).unwrap(), "segredo");
    }

    #[test]
    fn write_atomic_replaces_a_symlink_instead_of_following_it() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("alvo");
        fs::write(&target, "intacto").unwrap();
        let link = d.path().join("config.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&link, b"novo").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "intacto");
        assert_eq!(fs::read_to_string(&link).unwrap(), "novo");
        assert!(!fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn write_atomic_ignores_planted_names_sets_mode_and_leaves_no_temporaries() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.json");
        let victim = d.path().join("vitima");
        fs::write(&victim, "intacta").unwrap();
        // o usuário planta symlinks com os nomes "óbvios" do temporário antigo
        for suffix in [format!(".tmp{}", std::process::id()), ".tmp0-0".into(), ".tmp".into()] {
            let _ = std::os::unix::fs::symlink(&victim, d.path().join(format!("s.json{suffix}")));
        }
        write_atomic_mode(&path, b"{}", 0o600).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "intacta");
        let temps: Vec<_> = fs::read_dir(d.path()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("s.json.tmp") && !fs::symlink_metadata(d.path().join(n)).unwrap().file_type().is_symlink()).collect();
        assert!(temps.is_empty(), "sobrou temporário: {temps:?}");
    }

    #[test]
    fn concurrent_atomic_writes_never_collide_or_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("c.json");
        let hs: Vec<_> = (0..16).map(|i| {
            let p = path.clone();
            std::thread::spawn(move || {
                for n in 0..20 {
                    write_atomic(&p, format!("{{\"w\":{i},\"n\":{n}}}").as_bytes()).unwrap();
                }
            })
        }).collect();
        for h in hs {
            h.join().unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["n"].as_u64().is_some(), "o arquivo final é sempre um JSON inteiro");
    }

    #[test]
    fn read_small_accepts_only_small_regular_text_files() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("ok");
        fs::write(&ok, "abc").unwrap();
        assert_eq!(read_small(&ok, 3).as_deref(), Some("abc"));
        assert_eq!(read_small(&ok, 2), None, "grande demais");
        assert_eq!(read_small(d.path(), 100), None, "diretório");
        assert_eq!(read_small(d.path().join("nao-existe"), 100), None);
        let bin = d.path().join("bin");
        fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_small(&bin, 100), None, "não é UTF-8");
        let zero = d.path().join("zero");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert_eq!(read_small(&zero, 1 << 20), None, "/dev/zero não esgota a memória");
    }


    #[test]
    fn natural_sort_orders_numbers() {
        let mut v = vec!["hwmon10", "hwmon2", "hwmon1", "cpu0", "cpu11", "cpu9"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["cpu0", "cpu9", "cpu11", "hwmon1", "hwmon2", "hwmon10"]);
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
        assert_eq!(fmt_bytes(8 * 1024 * 1024 * 1024), "8.0 GiB");
        assert_eq!(fmt_bytes_si(500_107_862_016), "500 GB");
        assert_eq!(fmt_bytes_si(1_000_204_886_016), "1.0 TB");
    }

    #[test]
    fn formats_durations_and_clocks() {
        assert_eq!(fmt_duration(45), "45 s");
        assert_eq!(fmt_duration(125), "2 min 5 s");
        assert_eq!(fmt_duration(3 * 3600 + 120), "3 h 2 min");
        assert_eq!(fmt_duration(2 * 86400 + 3 * 3600 + 240), "2 d 3 h 4 min");
        assert_eq!(fmt_mhz(3600.0), "3.60 GHz");
        assert_eq!(fmt_mhz(800.0), "800 MHz");
    }

    #[test]
    fn text_bar_and_trunc() {
        assert_eq!(text_bar(50.0, 10), "█████░░░░░");
        assert_eq!(text_bar(150.0, 4), "████");
        assert_eq!(trunc("abcdef", 4), "abc…");
        assert_eq!(trunc("abc", 4), "abc");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc-1.2"), "abc-1.2");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(html_escape("<a href=\"x\">&</a>"), "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;");
    }

    #[test]
    fn parses_colon_and_env_files() {
        let kv = parse_colon_lines("MemTotal:       16384 kB\nMemFree: 100 kB\nlinha sem dois pontos");
        assert_eq!(kv[0], ("MemTotal".into(), "16384 kB".into()));
        assert_eq!(kv.len(), 2);
        let env = parse_env_file("# c\nNAME=\"Ubuntu\"\nPRETTY_NAME='Ubuntu 24.04'\nID=ubuntu\n");
        assert_eq!(env[0], ("NAME".into(), "Ubuntu".into()));
        assert_eq!(env[1].1, "Ubuntu 24.04");
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn passwd_lookup_finds_user() {
        let text = "root:x:0:0:root:/root:/bin/bash\nkaique:x:1000:1000:Kaique:/home/kaique:/bin/zsh\n";
        let u = passwd_lookup(text, 1000).unwrap();
        assert_eq!((u.name.as_str(), u.gid), ("kaique", 1000));
        assert_eq!(u.home, PathBuf::from("/home/kaique"));
        assert!(passwd_lookup(text, 4242).is_none());
    }

    #[test]
    fn runs_commands_with_timeout() {
        let o = run("sh", &["-c", "echo oi; echo erro >&2; exit 3"], Duration::from_secs(5)).unwrap();
        assert_eq!(o.status, Some(3));
        assert_eq!(o.stdout.trim(), "oi");
        assert_eq!(o.last_line(), "erro");
        let o = run("sleep", &["5"], Duration::from_millis(150)).unwrap();
        assert!(o.timed_out);
        assert!(run("programa-que-nao-existe-xyz", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn atomic_write_replaces_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/dir/a.txt");
        write_atomic(&p, b"um").unwrap();
        write_atomic(&p, b"dois").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "dois");
        let leftovers: Vec<_> = dir_names(p.parent().unwrap()).into_iter().filter(|n| n.contains(".tmp")).collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn tasks_report_progress_and_finish() {
        let mut t = spawn_task(|report: &dyn Fn(u32), _c: &Cancel| {
            report(1);
            report(2);
            "ok"
        });
        let mut got = Vec::new();
        let start = Instant::now();
        while !t.is_finished() && start.elapsed() < Duration::from_secs(5) {
            for m in t.poll() {
                got.push(match m {
                    TaskMsg::Progress(p) => format!("p{p}"),
                    TaskMsg::Done(r) => r.to_string(),
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got, ["p1", "p2", "ok"]);
        assert!(!t.crashed());
    }

    #[test]
    fn cancel_wakes_sleepers() {
        let c = Cancel::new();
        let c2 = c.clone();
        let h = thread::spawn(move || c2.sleep(Duration::from_secs(10)));
        thread::sleep(Duration::from_millis(50));
        c.cancel();
        assert!(h.join().unwrap());
    }
}
