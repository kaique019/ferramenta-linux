//! Interface de terminal (TUI). Tudo acontece dentro dela: abas com dados ao vivo, testes com progresso,
//! perfis de jogo e ferramentas. Só um jogo fora da Steam sai da tela cheia (e volta ao terminar).
//!
//! Regras que evitam os erros clássicos de TUI: o terminal é sempre restaurado (inclusive em panic), só eventos de
//! tecla pressionada contam, Ctrl-C cancela o teste ou sai, listas rolam em terminais pequenos e a coleta de dados
//! roda em threads para a tela nunca congelar.

pub mod data;
pub mod jobs;
pub mod theme;
mod ui;

use std::cell::Cell;
use std::io;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::games::{self, Game, Source};
use crate::settings::{GameProfile, StartupBackup};
use crate::util::{Task, TaskMsg, spawn_task};
use crate::{autostart, installer, launch, tweaks};
use data::{Env, Live, Msg, Static};
use jobs::{JobKind, Record, Running};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Resumo,
    Sensores,
    Discos,
    Gpu,
    Rede,
    Testes,
    Jogos,
    Ferramentas,
}

pub const TABS: [Tab; 8] = [Tab::Resumo, Tab::Sensores, Tab::Discos, Tab::Gpu, Tab::Rede, Tab::Testes, Tab::Jogos, Tab::Ferramentas];

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Resumo => "Resumo",
            Tab::Sensores => "Sensores",
            Tab::Discos => "Discos",
            Tab::Gpu => "GPU",
            Tab::Rede => "Energia e rede",
            Tab::Testes => "Testes",
            Tab::Jogos => "Jogos",
            Tab::Ferramentas => "Ferramentas",
        }
    }

    pub fn index(self) -> usize {
        TABS.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Ok,
    Err,
}

/// Jogo fora da Steam a executar: a interface sai da tela cheia, roda e volta.
pub struct LaunchReq {
    pub name: String,
    pub exe: String,
    pub args: String,
    pub profile: GameProfile,
}

pub enum Effect {
    None,
    Quit,
    Launch(Box<LaunchReq>),
}

/// Instalação em andamento: nome do programa e a tarefa que o instala.
pub type InstallTask = (String, Task<(), Result<(), String>>);

pub struct App {
    pub env: Env,
    pub tab: Tab,
    pub stat: Option<Box<Static>>,
    pub live: Option<Box<Live>>,
    pub cpu_history: Vec<u64>,
    pub scroll: [u16; 8],
    /// Rolagem máxima da aba atual, calculada ao desenhar.
    pub max_scroll: Cell<u16>,
    pub job_sel: usize,
    pub job: Option<Running>,
    pub job_result: Option<(JobKind, jobs::Done)>,
    pub games: Vec<Game>,
    pub game_sel: usize,
    pub profiles: std::collections::BTreeMap<String, GameProfile>,
    pub tool_panel: usize,
    pub tool_sel: [usize; 3],
    pub tweaks: Vec<tweaks::TweakStatus>,
    pub startup: Vec<autostart::AutostartEntry>,
    pub removed: Vec<StartupBackup>,
    pub apps: Vec<installer::AppStatus>,
    pub install: Option<InstallTask>,
    pub status: Option<(Level, String)>,
    pub theme: theme::Theme,
    pub quit: bool,
}

fn step(sel: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        0
    } else if down {
        (sel + 1).min(len - 1)
    } else {
        sel.saturating_sub(1)
    }
}

impl App {
    pub fn new(env: Env) -> App {
        let mut app = App {
            env,
            tab: Tab::Resumo,
            stat: None,
            live: None,
            cpu_history: vec![],
            scroll: [0; 8],
            max_scroll: Cell::new(0),
            job_sel: 0,
            job: None,
            job_result: None,
            games: vec![],
            game_sel: 0,
            profiles: Default::default(),
            tool_panel: 0,
            tool_sel: [0; 3],
            tweaks: vec![],
            startup: vec![],
            removed: vec![],
            apps: vec![],
            install: None,
            status: None,
            theme: theme::Theme::default(),
            quit: false,
        };
        app.reload_theme();
        app.reload_games();
        app.reload_tools();
        app
    }

    pub fn set_status(&mut self, level: Level, text: impl Into<String>) {
        self.status = Some((level, text.into()));
    }

    pub fn apply(&mut self, m: Msg) {
        match m {
            Msg::Static(s) => self.stat = Some(s),
            Msg::Live(l) => {
                self.cpu_history.push(l.cpu.total.clamp(0.0, 100.0) as u64);
                if self.cpu_history.len() > 120 {
                    self.cpu_history.remove(0);
                }
                self.live = Some(l);
            }
        }
    }

    /// Lê o `theme.json`; se estiver errado, mantém o tema padrão e mostra o motivo.
    pub fn reload_theme(&mut self) {
        let (theme, warning) = theme::load(&self.env.theme_path);
        self.theme = theme;
        match warning {
            Some(w) => self.set_status(Level::Err, w),
            None => self.set_status(Level::Info, format!("tema: {}", self.theme.name)),
        }
    }

    pub fn reload_games(&mut self) {
        let st = self.env.store.load().settings;
        self.games = games::detect_all(&self.env.home, &st.custom_games);
        self.profiles = st.game_profiles;
        self.game_sel = self.game_sel.min(self.games.len().saturating_sub(1));
    }

    pub fn reload_tools(&mut self) {
        let st = self.env.store.load().settings;
        self.tweaks = tweaks::status(&self.env.roots, &st.tweak_backup);
        self.startup = autostart::list(&self.env.autostart);
        self.removed = st.startup_removed;
        self.apps = installer::catalog();
        for (i, len) in [self.tweaks.len(), self.startup.len(), self.apps.len()].into_iter().enumerate() {
            self.tool_sel[i] = self.tool_sel[i].min(len.saturating_sub(1));
        }
    }

    pub fn profile_of(&self, g: &Game) -> GameProfile {
        self.profiles.get(&g.id).cloned().unwrap_or_default()
    }

    /// Trabalhos em segundo plano: avança o teste em curso e a instalação.
    pub fn tick(&mut self) {
        if let Some(job) = &mut self.job {
            let mut finished = None;
            for m in job.task.poll() {
                match m {
                    TaskMsg::Progress(p) => job.last = p,
                    TaskMsg::Done(r) => finished = Some(r),
                }
            }
            let crashed = job.task.crashed();
            let kind = job.kind;
            if finished.is_some() || crashed {
                self.job = None;
                match finished {
                    Some(Ok(done)) => {
                        let (ok, cancelled) = (done.ok, done.cancelled);
                        if let Some(rec) = &done.record {
                            let saved = self.env.store.update(|s| match rec {
                                Record::Run(r) => s.push_bench_run(r.clone()),
                                Record::Bench(b) => s.push_bench_record(b.clone()),
                                Record::Oc(o) => s.push_oc_record(o.clone()),
                            });
                            if let Err(e) = saved {
                                self.set_status(Level::Err, format!("o resultado não pôde ser salvo: {e}"));
                            }
                        }
                        if self.status.as_ref().is_none_or(|(l, _)| *l != Level::Err) {
                            let (level, what) = if cancelled { (Level::Info, "cancelado") } else if ok { (Level::Ok, "concluído") } else { (Level::Err, "concluído com problemas") };
                            self.set_status(level, format!("{}: {what}", kind.title()));
                        }
                        self.job_result = Some((kind, done));
                    }
                    Some(Err(e)) => self.set_status(Level::Err, format!("{} falhou: {e}", kind.title())),
                    None => self.set_status(Level::Err, format!("{} parou de forma inesperada", kind.title())),
                }
            }
        }
        if let Some((name, task)) = &mut self.install {
            let mut done = None;
            for m in task.poll() {
                if let TaskMsg::Done(r) = m {
                    done = Some(r);
                }
            }
            if done.is_some() || task.crashed() {
                let name = name.clone();
                self.install = None;
                match done {
                    Some(Ok(())) => self.set_status(Level::Ok, format!("{name} instalado")),
                    Some(Err(e)) => self.set_status(Level::Err, format!("não instalou {name}: {e}")),
                    None => self.set_status(Level::Err, format!("a instalação de {name} parou de forma inesperada")),
                }
                self.reload_tools();
            }
        }
    }

    pub fn cancel_job(&mut self) -> bool {
        match &self.job {
            Some(j) => {
                j.task.cancel.cancel();
                self.set_status(Level::Info, "cancelando...");
                true
            }
            None => false,
        }
    }

    fn goto(&mut self, t: Tab) {
        self.tab = t;
        self.max_scroll.set(0);
    }

    fn rotate(&mut self, forward: bool) {
        let i = self.tab.index();
        let n = TABS.len();
        self.goto(TABS[if forward { (i + 1) % n } else { (i + n - 1) % n }]);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Effect {
        if key.kind != KeyEventKind::Press {
            return Effect::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if !self.cancel_job() {
                    self.quit = true;
                    return Effect::Quit;
                }
                return Effect::None;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.cancel_job();
                self.quit = true;
                return Effect::Quit;
            }
            KeyCode::Esc => {
                if !self.cancel_job() {
                    self.status = None;
                }
                return Effect::None;
            }
            KeyCode::Char('t') if !ctrl => {
                self.theme = self.theme.next_preset();
                self.set_status(Level::Info, format!("tema: {} (temporário; fixe com `pingu theme --preset {} --write`)", self.theme.name, self.theme.name));
                return Effect::None;
            }
            KeyCode::Char('T') => {
                self.reload_theme();
                return Effect::None;
            }
            KeyCode::Tab => {
                self.rotate(true);
                return Effect::None;
            }
            KeyCode::BackTab => {
                self.rotate(false);
                return Effect::None;
            }
            KeyCode::Char(c @ '1'..='8') if !ctrl => {
                self.goto(TABS[c as usize - '1' as usize]);
                return Effect::None;
            }
            _ => {}
        }
        match self.tab {
            Tab::Testes => self.key_tests(key),
            Tab::Jogos => return self.key_games(key),
            Tab::Ferramentas => self.key_tools(key),
            _ => self.key_scroll(key),
        }
        Effect::None
    }

    fn key_scroll(&mut self, key: KeyEvent) {
        let i = self.tab.index();
        let max = self.max_scroll.get();
        let s = &mut self.scroll[i];
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => *s = (*s + 1).min(max),
            KeyCode::Up | KeyCode::Char('k') => *s = s.saturating_sub(1),
            KeyCode::PageDown => *s = (*s + 10).min(max),
            KeyCode::PageUp => *s = s.saturating_sub(10),
            KeyCode::Home => *s = 0,
            KeyCode::End => *s = max,
            KeyCode::Right | KeyCode::Char('l') => self.rotate(true),
            KeyCode::Left | KeyCode::Char('h') => self.rotate(false),
            _ => {}
        }
    }

    fn key_tests(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.job_sel = step(self.job_sel, jobs::ALL.len(), true),
            KeyCode::Up | KeyCode::Char('k') => self.job_sel = step(self.job_sel, jobs::ALL.len(), false),
            KeyCode::Right | KeyCode::Char('l') => self.rotate(true),
            KeyCode::Left | KeyCode::Char('h') => self.rotate(false),
            KeyCode::Enter => {
                if self.job.is_some() {
                    self.set_status(Level::Info, "já há um teste em andamento (Esc cancela)");
                } else {
                    let kind = jobs::ALL[self.job_sel];
                    self.job_result = None;
                    self.status = None;
                    self.job = Some(jobs::start(kind, &self.env));
                }
            }
            _ => {}
        }
    }

    fn edit_profile(&mut self, f: impl FnOnce(&mut GameProfile)) {
        let Some(g) = self.games.get(self.game_sel) else { return };
        let id = g.id.clone();
        match self.env.store.update(|s| f(s.game_profiles.entry(id).or_default())) {
            Ok(()) => self.reload_games(),
            Err(e) => self.set_status(Level::Err, format!("não consegui salvar o perfil: {e}")),
        }
    }

    fn key_games(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.game_sel = step(self.game_sel, self.games.len(), true),
            KeyCode::Up | KeyCode::Char('k') => self.game_sel = step(self.game_sel, self.games.len(), false),
            KeyCode::Right | KeyCode::Char('l') => self.rotate(true),
            KeyCode::Left | KeyCode::Char('h') => self.rotate(false),
            KeyCode::Char('g') => self.edit_profile(|p| p.gamemode = !p.gamemode),
            KeyCode::Char('m') => self.edit_profile(|p| p.mangohud = !p.mangohud),
            KeyCode::Char('d') => self.edit_profile(|p| p.dgpu = !p.dgpu),
            KeyCode::Char('p') => self.edit_profile(|p| p.performance_governor = !p.performance_governor),
            KeyCode::Char('i') => self.edit_profile(|p| p.priority = match p.priority.as_str() { "normal" => "above", "above" => "high", _ => "normal" }.into()),
            KeyCode::Char('r') => {
                self.reload_games();
                self.set_status(Level::Info, format!("{} jogos encontrados", self.games.len()));
            }
            KeyCode::Enter => {
                let Some(g) = self.games.get(self.game_sel).cloned() else { return Effect::None };
                if g.source == Source::Steam {
                    self.set_status(Level::Info, "Steam: cole as opções mostradas ao lado em Propriedades > Opções de inicialização");
                    return Effect::None;
                }
                let profile = self.profile_of(&g);
                let Some(exe) = profile.exe_path.clone().or(g.exe.clone()) else {
                    self.set_status(Level::Err, "este jogo não tem executável conhecido");
                    return Effect::None;
                };
                let st = self.env.store.load().settings;
                let args = st.custom_games.iter().find(|c| c.exe == exe).map(|c| c.args.clone()).unwrap_or_default();
                return Effect::Launch(Box::new(LaunchReq { name: g.name, exe, args, profile }));
            }
            _ => {}
        }
        Effect::None
    }

    fn key_tools(&mut self, key: KeyEvent) {
        let lens = [self.tweaks.len(), self.startup.len(), self.apps.len()];
        let p = self.tool_panel;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.tool_sel[p] = step(self.tool_sel[p], lens[p], true),
            KeyCode::Up | KeyCode::Char('k') => self.tool_sel[p] = step(self.tool_sel[p], lens[p], false),
            KeyCode::Right | KeyCode::Char('l') => {
                if p < 2 { self.tool_panel += 1 } else { self.rotate(true) }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if p > 0 { self.tool_panel -= 1 } else { self.rotate(false) }
            }
            KeyCode::Char('r') => {
                self.reload_tools();
                self.set_status(Level::Info, "atualizado");
            }
            KeyCode::Enter => self.tool_action(),
            _ => {}
        }
    }

    fn tool_action(&mut self) {
        let sel = self.tool_sel[self.tool_panel];
        match self.tool_panel {
            0 => {
                let Some(t) = self.tweaks.get(sel).cloned() else { return };
                let roots = self.env.roots.clone();
                let res = if t.applied && t.can_restore {
                    self.env.store.update(|s| tweaks::restore(&roots, &mut s.tweak_backup, t.id)).map(|r| r.map(|wrote| if wrote { format!("{} restaurado", t.id) } else { format!("{}: o computador reiniciou, já voltou ao padrão", t.id) }))
                } else if t.applied {
                    self.set_status(Level::Info, "já está no valor recomendado e não há original guardado");
                    return;
                } else if !t.available {
                    self.set_status(Level::Err, "este kernel não oferece esse ajuste");
                    return;
                } else {
                    self.env.store.update(|s| tweaks::apply(&roots, &mut s.tweak_backup, t.id)).map(|r| r.map(|_| format!("{} aplicado (vale até reiniciar)", t.id)))
                };
                match res {
                    Ok(Ok(msg)) => self.set_status(Level::Ok, msg),
                    Ok(Err(e)) => self.set_status(Level::Err, e),
                    Err(e) => self.set_status(Level::Err, format!("não consegui salvar as configurações: {e}")),
                }
            }
            1 => {
                let Some(e) = self.startup.get(sel).cloned() else { return };
                if e.enabled {
                    match autostart::disable(&self.env.autostart, &e.file) {
                        Ok(b) => match self.env.store.update(|s| s.startup_removed.push(b)) {
                            Ok(()) => self.set_status(Level::Ok, format!("{} desativado (Enter de novo restaura)", e.name)),
                            Err(err) => self.set_status(Level::Err, format!("desativado, mas o registro de backup falhou: {err}")),
                        },
                        Err(err) => self.set_status(Level::Err, err),
                    }
                } else if let Some(b) = self.removed.iter().find(|b| b.original.ends_with(&format!("/{}", e.file))).cloned() {
                    match autostart::restore(&self.env.autostart, &b) {
                        Ok(()) => {
                            let orig = b.original.clone();
                            let _ = self.env.store.update(|s| s.startup_removed.retain(|x| x.original != orig));
                            self.set_status(Level::Ok, format!("{} restaurado", e.name));
                        }
                        Err(err) => self.set_status(Level::Err, err),
                    }
                } else {
                    self.set_status(Level::Info, "foi desativado por outro programa; não há backup aqui para restaurar");
                }
            }
            _ => {
                let Some(a) = self.apps.get(sel).cloned() else { return };
                if a.installed {
                    self.set_status(Level::Info, format!("{} já está instalado", a.id));
                } else if self.install.is_some() {
                    self.set_status(Level::Info, "já há uma instalação em andamento");
                } else {
                    let roots = self.env.roots.clone();
                    let id = a.id.to_string();
                    self.set_status(Level::Info, format!("instalando {id}... (precisa de root ou sudo já autorizado)"));
                    self.install = Some((a.id.to_string(), spawn_task(move |_, _| installer::install(&roots, &id))));
                }
            }
        }
        self.reload_tools();
    }
}

/// Executa um jogo fora da Steam: sai da tela cheia, roda com o perfil e volta, mostrando o resultado.
fn run_game_outside(req: &LaunchReq, app: &mut App) {
    ratatui::restore();
    println!("Iniciando {}... (Ctrl-C encerra o jogo)\n", req.name);
    let dgpu = app.live.as_ref().and_then(|l| crate::gpu::primary(&l.gpus)).map(|g| g.vendor);
    let cancel = crate::cli::cancel_on_ctrl_c();
    let msg = match launch::run(&req.profile, &req.exe, &req.args, dgpu, &cancel) {
        Ok(o) => {
            for w in &o.warnings {
                println!("aviso: {w}");
            }
            if o.cancelled { "Encerrado.".to_string() } else { format!("{} terminou (código {}).", req.name, o.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())) }
        }
        Err(e) => format!("Erro: {e}"),
    };
    println!("\n{msg}\nPressione Enter para voltar...");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    app.set_status(Level::Info, msg);
}

fn event_loop(app: &mut App, rx: &Receiver<Msg>) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> io::Result<()> {
        loop {
            while let Ok(m) = rx.try_recv() {
                app.apply(m);
            }
            app.tick();
            terminal.draw(|f| ui::draw(f, app))?;
            if app.quit {
                return Ok(());
            }
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(k) = event::read()? {
                    match app.on_key(k) {
                        Effect::Quit => return Ok(()),
                        Effect::Launch(req) => {
                            run_game_outside(&req, app);
                            terminal = ratatui::init();
                        }
                        Effect::None => {}
                    }
                }
        }
    })();
    ratatui::restore();
    result
}

pub fn run() -> io::Result<()> {
    use std::io::IsTerminal;
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        return Err(io::Error::other("a interface precisa de um terminal interativo; use um subcomando (veja --help)"));
    }
    let env = Env::system();
    let mut app = App::new(env.clone());
    let rx = data::spawn_collectors(env, Duration::from_secs(2));
    event_loop(&mut app, &rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::Source;
    use crate::sensors::SensorKind;
    use crate::settings::{CustomGame, SettingsStore};
    use crate::util::{Cancel, Roots};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::path::Path;

    fn test_env(d: &Path) -> Env {
        Env { roots: Roots::under(d), store: SettingsStore::new(d.join("cfg/settings.json")), autostart: autostart::Dirs { user: d.join("as"), system: vec![d.join("sysas")], backup: d.join("bk") }, home: d.join("home"), theme_path: d.join("cfg/theme.json") }
    }

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn text_of(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    fn app_with_data(d: &Path) -> App {
        let env = test_env(d);
        let mut app = App::new(env.clone());
        app.apply(Msg::Static(Box::new(data::collect_static(&env))));
        let mut s = crate::sensors::CpuSampler::new(&env.roots);
        app.apply(Msg::Live(Box::new(data::collect_live(&env, &mut s))));
        app
    }

    #[test]
    fn every_tab_renders_at_many_sizes_without_panicking() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        for tab in TABS {
            app.tab = tab;
            for (w, h) in [(80, 24), (60, 14), (200, 60), (59, 14), (80, 13), (10, 3), (1, 1)] {
                let _ = text_of(&app, w, h);
            }
        }
        // sem dados ainda: mostra "coletando", não quebra
        let empty = App::new(test_env(d.path()));
        assert!(text_of(&empty, 100, 30).contains("Coletando"));
    }

    #[test]
    fn small_terminal_shows_a_message_instead_of_garbage() {
        let d = tempfile::tempdir().unwrap();
        let app = App::new(test_env(d.path()));
        let t = text_of(&app, 40, 10);
        assert!(t.contains("Terminal pequeno") && t.contains("60x14"));
    }

    #[test]
    fn tab_navigation_wraps_and_digits_jump() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Sensores);
        app.on_key(key(KeyCode::BackTab));
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Ferramentas, "volta do início para o fim");
        app.on_key(key(KeyCode::Char('6')));
        assert_eq!(app.tab, Tab::Testes);
        app.on_key(key(KeyCode::Char('9')));
        assert_eq!(app.tab, Tab::Testes, "9 não existe");
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.tab, Tab::Jogos);
    }

    #[test]
    fn only_key_presses_count_and_quit_works() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        let mut release = key(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert!(matches!(app.on_key(release), Effect::None) && !app.quit);
        assert!(matches!(app.on_key(key(KeyCode::Char('q'))), Effect::Quit) && app.quit);
        let mut app = App::new(test_env(d.path()));
        assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Effect::Quit));
    }

    fn fake_job(app: &mut App, kind: JobKind, f: impl FnOnce(&dyn Fn(jobs::Prog), &Cancel) -> Result<jobs::Done, String> + Send + 'static) {
        app.job = Some(Running { kind, task: spawn_task(f), render: None, sim: None, last: Default::default(), started: std::time::Instant::now() });
    }

    fn wait_job(app: &mut App) {
        // folga grande: com a suíte inteira em paralelo a thread do teste pode demorar para ser escalonada
        for _ in 0..1500 {
            app.tick();
            if app.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("o teste não terminou");
    }

    #[test]
    fn finished_job_saves_its_record_and_reports_status() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        fake_job(&mut app, JobKind::Sim, |rep, _| {
            rep(jobs::Prog { label: "meio".into(), fraction: 0.5 });
            Ok(jobs::Done { lines: vec!["60 FPS".into()], record: Some(Record::Bench(crate::settings::BenchRecord { date: 1, test: "sim".into(), score: 60.0, summary: "60 FPS".into() })), ok: true, cancelled: false })
        });
        wait_job(&mut app);
        assert!(matches!(app.status, Some((Level::Ok, _))));
        assert_eq!(app.env.store.load().settings.bench_records.len(), 1);
        app.tab = Tab::Testes;
        assert!(text_of(&app, 100, 30).contains("60 FPS"));
    }

    #[test]
    fn escape_and_ctrl_c_cancel_a_running_job_without_quitting() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        fake_job(&mut app, JobKind::Stress, |_, cancel| {
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(jobs::Done { lines: vec!["parcial".into()], record: None, ok: false, cancelled: true })
        });
        assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Effect::None));
        assert!(!app.quit, "Ctrl-C com teste rodando só cancela o teste");
        wait_job(&mut app);
        assert!(matches!(app.status, Some((Level::Info, ref s)) if s.contains("cancelado")), "{:?}", app.status);
        app.on_key(key(KeyCode::Esc));
        assert!(app.status.is_none());
    }

    #[test]
    fn crashed_job_is_reported_not_hidden() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        fake_job(&mut app, JobKind::Memory, |_, _| panic!("boom"));
        wait_job(&mut app);
        assert!(matches!(app.status, Some((Level::Err, _))));
    }

    #[test]
    fn cannot_start_two_jobs_at_once() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        app.tab = Tab::Testes;
        fake_job(&mut app, JobKind::Stress, |_, c| {
            while !c.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(jobs::Done { lines: vec![], record: None, ok: false, cancelled: true })
        });
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.status, Some((Level::Info, ref s)) if s.contains("em andamento")));
        app.cancel_job();
        wait_job(&mut app);
    }

    #[test]
    fn text_tabs_scroll_within_bounds() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        let mut live = app.live.clone().unwrap();
        for i in 0..60 {
            live.sensors.push(crate::sensors::SensorReading { chip: "x".into(), chip_display: "Chip".into(), kind: SensorKind::Temp, label: format!("t{i}"), value: 40.0, max: None, crit: None });
        }
        app.live = Some(live);
        app.tab = Tab::Sensores;
        let _ = text_of(&app, 80, 24);
        let max = app.max_scroll.get();
        assert!(max > 30, "{max}");
        for _ in 0..200 {
            app.on_key(key(KeyCode::Down));
        }
        assert_eq!(app.scroll[Tab::Sensores.index()], max);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.scroll[Tab::Sensores.index()], max - 1, "uma tecla para cima já sobe (sem rolar 'no vazio')");
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.scroll[Tab::Sensores.index()], 0);
        app.on_key(key(KeyCode::End));
        assert!(text_of(&app, 80, 24).contains("t59"));
    }

    #[test]
    fn game_profile_toggles_persist_and_steam_shows_options() {
        let d = tempfile::tempdir().unwrap();
        let env = test_env(d.path());
        put(&env.home.join(".local/share/Steam/steamapps/appmanifest_620.acf"), "\"AppState\" { \"appid\" \"620\" \"name\" \"Portal 2\" \"installdir\" \"P\" }");
        env.store.update(|s| s.custom_games.push(CustomGame { name: "Zeta".into(), exe: "/bin/true".into(), args: "--x".into() })).unwrap();
        let mut app = App::new(env);
        assert_eq!(app.games.len(), 2);
        app.tab = Tab::Jogos;
        assert_eq!(app.games[0].source, Source::Steam);
        app.on_key(key(KeyCode::Char('g')));
        app.on_key(key(KeyCode::Char('i')));
        let p = app.profile_of(&app.games[0].clone());
        assert!(p.gamemode && p.priority == "above");
        assert!(app.env.store.load().settings.game_profiles.contains_key("steam:620"), "salvo no disco");
        app.on_key(key(KeyCode::Char('g')));
        assert!(!app.profile_of(&app.games[0].clone()).gamemode);
        assert!(text_of(&app, 120, 30).contains("%command%"));
        assert!(matches!(app.on_key(key(KeyCode::Enter)), Effect::None), "Steam não lança por aqui");
        app.on_key(key(KeyCode::Down));
        match app.on_key(key(KeyCode::Enter)) {
            Effect::Launch(r) => assert_eq!((r.name.as_str(), r.exe.as_str(), r.args.as_str()), ("Zeta", "/bin/true", "--x")),
            _ => panic!("devia pedir para lançar"),
        }
    }

    #[test]
    fn tools_apply_and_restore_tweaks_and_startup() {
        let d = tempfile::tempdir().unwrap();
        let env = test_env(d.path());
        put(&env.roots.proc.join("sys/vm/swappiness"), "60\n");
        put(&env.autostart.user.join("discord.desktop"), "[Desktop Entry]\nName=Discord\nExec=discord\n");
        let mut app = App::new(env.clone());
        app.tab = Tab::Ferramentas;
        let idx = app.tweaks.iter().position(|t| t.id == "swappiness").unwrap();
        app.tool_sel[0] = idx;
        app.on_key(key(KeyCode::Enter));
        assert_eq!(fs::read_to_string(env.roots.proc.join("sys/vm/swappiness")).unwrap().trim(), "10");
        assert!(app.tweaks[idx].applied && app.tweaks[idx].can_restore);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(fs::read_to_string(env.roots.proc.join("sys/vm/swappiness")).unwrap().trim(), "60", "segundo Enter restaura");
        // inicialização: desativa e restaura
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.tool_panel, 1);
        app.on_key(key(KeyCode::Enter));
        assert!(!env.autostart.user.join("discord.desktop").exists());
        assert_eq!(app.removed.len(), 1);
        assert!(!app.startup.iter().any(|e| e.enabled), "some da lista de ativos");
        let _ = text_of(&app, 100, 30);
    }

    #[test]
    fn unavailable_tweak_gives_a_clear_message() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        app.tab = Tab::Ferramentas;
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.status, Some((Level::Err, ref s)) if s.contains("não oferece")), "{:?}", app.status);
    }

    fn fg_of(app: &App, w: u16, h: u16, needle: &str) -> Option<ratatui::style::Color> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        for y in 0..h {
            let row: Vec<String> = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if let Some(x) = row.join("").find(needle) {
                let col = row.join("")[..x].chars().count() as u16;
                return Some(buf[(col, y)].fg);
            }
        }
        None
    }

    #[test]
    fn theme_file_changes_colors_and_a_broken_one_falls_back_with_a_message() {
        use ratatui::style::Color;
        let d = tempfile::tempdir().unwrap();
        let env = test_env(d.path());
        // padrão: o rótulo "Kernel" do resumo usa o cinza do tema
        let app = app_with_data(d.path());
        assert_eq!(fg_of(&app, 100, 30, "Kernel"), Some(Color::DarkGray));
        // tema personalizado: o mesmo rótulo passa para a cor do arquivo
        put(&env.theme_path, r##"{"preset":"padrao","accent":"#ff8800","muted":"#112233","rounded_borders":true}"##);
        let mut app = app_with_data(d.path());
        assert_eq!(app.theme.accent, Color::Rgb(255, 136, 0));
        assert_eq!(app.theme.name, "padrao+");
        assert_eq!(fg_of(&app, 100, 30, "Kernel"), Some(Color::Rgb(0x11, 0x22, 0x33)));
        app.tab = Tab::Gpu;
        let t = text_of(&app, 100, 30);
        assert!(t.contains('╭'), "bordas arredondadas aplicadas");
        // arquivo quebrado: tema padrão e aviso na barra de status
        put(&env.theme_path, r#"{"accent":"vermelho"}"#);
        app.on_key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT));
        assert_eq!(app.theme, theme::Theme::default());
        assert!(matches!(app.status, Some((Level::Err, ref s)) if s.contains("accent")), "{:?}", app.status);
        // corrigido: T recarrega
        put(&env.theme_path, r#"{"preset":"neon"}"#);
        app.on_key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT));
        assert_eq!(app.theme.name, "neon");
    }

    #[test]
    fn t_key_cycles_presets_without_touching_the_file() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        let first = app.theme.name.clone();
        for _ in 0..theme::PRESETS.len() {
            app.on_key(key(KeyCode::Char('t')));
        }
        assert_eq!(app.theme.name, first, "dá a volta");
        assert!(!app.env.theme_path.exists());
        app.on_key(key(KeyCode::Char('t')));
        assert_ne!(app.theme.name, first);
        let _ = text_of(&app, 100, 30);
    }

    #[test]
    fn every_preset_renders_every_tab() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        for p in theme::PRESETS {
            app.theme = theme::preset(p).unwrap();
            for tab in TABS {
                app.tab = tab;
                let _ = text_of(&app, 90, 28);
            }
        }
    }

    #[test]
    fn terminal_is_not_required_for_state_but_run_refuses_without_one() {
        // sem terminal interativo (como no cargo test), run() recusa em vez de quebrar a tela
        let e = run().unwrap_err();
        assert!(e.to_string().contains("terminal interativo"));
    }
}
