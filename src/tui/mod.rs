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

use std::cell::{Cell, RefCell};
use std::io;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

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
    Processos,
}

pub const TABS: [Tab; 9] = [Tab::Resumo, Tab::Sensores, Tab::Discos, Tab::Gpu, Tab::Rede, Tab::Testes, Tab::Jogos, Tab::Ferramentas, Tab::Processos];

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
            Tab::Processos => "Processos",
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

/// Qual lista da tela um clique do mouse acertou.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListId {
    Jobs,
    Games,
    Tool(usize),
}

/// Áreas clicáveis do último quadro desenhado (preenchidas por `ui::draw`).
#[derive(Default, Clone, Debug)]
pub struct Hits {
    pub tabs: Vec<(Rect, Tab)>,
    /// área interna da lista, qual lista, primeiro item visível e quantidade de itens
    pub lists: Vec<(Rect, ListId, usize, usize)>,
    /// botões que equivalem a apertar uma tecla (ex.: 'g' liga o GameMode)
    pub buttons: Vec<(Rect, char)>,
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

pub struct App {
    pub env: Env,
    pub tab: Tab,
    pub stat: Option<Box<Static>>,
    pub live: Option<Box<Live>>,
    pub cpu_history: Vec<u64>,
    pub mem_history: Vec<u64>,
    pub scroll: [u16; 9],
    pub proc_sort: crate::procs::SortBy,
    /// janela de ajuda aberta (F1 ou ?)
    pub help: bool,
    pub hits: RefCell<Hits>,
    last_click: Option<(std::time::Instant, u16, u16)>,
    /// Rolagem máxima da aba atual, calculada ao desenhar.
    pub max_scroll: Cell<u16>,
    /// densidade do último quadro desenhado (preenchida por `ui::draw`)
    pub dense: Cell<ui::Dense>,
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

fn key_press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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
            mem_history: vec![],
            proc_sort: Default::default(),
            help: false,
            hits: RefCell::new(Hits::default()),
            last_click: None,
            scroll: [0; 9],
            max_scroll: Cell::new(0),
            dense: Cell::new(ui::Dense::default()),
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
                let mem_pct = if l.mem.total > 0 { l.mem.total.saturating_sub(l.mem.available) as f64 / l.mem.total as f64 * 100.0 } else { 0.0 };
                self.mem_history.push(mem_pct.clamp(0.0, 100.0) as u64);
                for h in [&mut self.cpu_history, &mut self.mem_history] {
                    if h.len() > 120 {
                        h.remove(0);
                    }
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
        if self.help {
            // a ajuda fecha com qualquer tecla; Ctrl-C ainda sai
            if matches!(key.code, KeyCode::Char('c') if ctrl) {
                self.quit = true;
                return Effect::Quit;
            }
            self.help = false;
            return Effect::None;
        }
        match key.code {
            KeyCode::F(1) | KeyCode::Char('?') => {
                self.help = true;
                return Effect::None;
            }
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
            KeyCode::Char(c @ '1'..='9') if !ctrl => {
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
            KeyCode::Char('c') if self.tab == Tab::Processos => self.proc_sort = crate::procs::SortBy::Cpu,
            KeyCode::Char('m') if self.tab == Tab::Processos => self.proc_sort = crate::procs::SortBy::Mem,
            _ => {}
        }
    }

    /// Mouse: roda rola ou muda a seleção, clique em aba/lista/botão faz o mesmo que o teclado,
    /// duplo clique em teste ou jogo inicia (como Enter).
    pub fn on_mouse(&mut self, m: MouseEvent) -> Effect {
        if self.help {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.help = false;
            }
            return Effect::None;
        }
        let hits = self.hits.borrow().clone();
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = m.kind == MouseEventKind::ScrollDown;
                if let Some((_, ListId::Tool(i), ..)) = hits.lists.iter().find(|(r, ..)| inside(*r, x, y)) {
                    self.tool_panel = *i;
                }
                let code = if down { KeyCode::Down } else { KeyCode::Up };
                // texto rola de 3 em 3 linhas; listas andam de 1 em 1
                let n = if matches!(self.tab, Tab::Testes | Tab::Jogos | Tab::Ferramentas) { 1 } else { 3 };
                for _ in 0..n {
                    self.on_key(key_press(code));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, t)) = hits.tabs.iter().find(|(r, _)| inside(*r, x, y)) {
                    self.goto(*t);
                    return Effect::None;
                }
                if let Some((_, c)) = hits.buttons.iter().find(|(r, _)| inside(*r, x, y)) {
                    return self.on_key(key_press(KeyCode::Char(*c)));
                }
                if let Some((r, id, off, len)) = hits.lists.iter().find(|(r, ..)| inside(*r, x, y)) {
                    let idx = off + (y - r.y) as usize;
                    if idx < *len {
                        match id {
                            ListId::Jobs => self.job_sel = idx,
                            ListId::Games => self.game_sel = idx,
                            ListId::Tool(i) => {
                                self.tool_panel = *i;
                                self.tool_sel[*i] = idx;
                            }
                        }
                        let now = std::time::Instant::now();
                        let double = self.last_click.is_some_and(|(t, cx, cy)| cx == x && cy == y && now.duration_since(t) < Duration::from_millis(450));
                        self.last_click = Some((now, x, y));
                        // duplo clique age como Enter, mas só em testes e jogos (nunca aplica ajustes do sistema por engano)
                        if double && matches!(id, ListId::Jobs | ListId::Games) {
                            self.last_click = None;
                            return self.on_key(key_press(KeyCode::Enter));
                        }
                    }
                }
            }
            _ => {}
        }
        Effect::None
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
                // jogos do Heroic abrem pelo próprio Heroic (ele cuida do Wine/Proton); um executável definido à mão vence
                let via_heroic = if profile.exe_path.is_none() { games::heroic_launch(&g, &self.env.home, &crate::util::have) } else { None };
                let (exe, args) = if let Some((exe, args)) = via_heroic {
                    (exe, args)
                } else {
                    let Some(exe) = profile.exe_path.clone().or(g.exe.clone()) else {
                        self.set_status(Level::Err, "este jogo não tem executável conhecido (para jogos do Heroic, instale ou abra o Heroic)");
                        return Effect::None;
                    };
                    let st = self.env.store.load().settings;
                    let args = st.custom_games.iter().find(|c| c.exe == exe).map(|c| c.args.clone()).unwrap_or_default();
                    (exe, args)
                };
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
    restore_term();
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

/// Entra na tela cheia com captura de mouse; se o programa cair (panic), o mouse também é devolvido.
fn init_term() -> ratatui::DefaultTerminal {
    static HOOK: std::sync::Once = std::sync::Once::new();
    let t = ratatui::init();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture);
            prev(info);
        }));
    });
    let _ = ratatui::crossterm::execute!(io::stdout(), EnableMouseCapture);
    t
}

fn restore_term() {
    let _ = ratatui::crossterm::execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

fn event_loop(app: &mut App, rx: &Receiver<Msg>) -> io::Result<()> {
    let mut terminal = init_term();
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
            if event::poll(Duration::from_millis(100))? {
                let effect = match event::read()? {
                    Event::Key(k) => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => Effect::None,
                };
                match effect {
                    Effect::Quit => return Ok(()),
                    Effect::Launch(req) => {
                        run_game_outside(&req, app);
                        terminal = init_term();
                    }
                    Effect::None => {}
                }
            }
        }
    })();
    restore_term();
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
        let mut s = data::Samplers::new(&env.roots);
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
    fn layout_adapts_to_the_terminal_instead_of_giving_up() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        // 40x10 antes só mostrava "terminal pequeno"; agora desenha a aba com o layout mínimo
        app.tab = Tab::Resumo;
        let t = text_of(&app, 40, 10);
        assert!(!t.contains("Janela pequena"), "{t}");
        assert!(t.contains("CPU") && t.contains("RAM"), "{t}");
        // estreito: painéis empilhados (Agora acima de Sistema), todos os botões dos jogos um por linha
        let t = text_of(&app, 70, 30);
        assert!(t.find("Agora").unwrap() < t.find("Sistema").unwrap(), "{t}");
        // largo: lado a lado (Sistema à esquerda, na mesma linha de Agora)
        let t = text_of(&app, 120, 30);
        let line = t.lines().find(|l| l.contains("Sistema")).unwrap();
        assert!(line.contains("Agora"), "{t}");
        // testes: lista e detalhes continuam acessíveis em 50x14
        app.tab = Tab::Testes;
        let t = text_of(&app, 50, 14);
        assert!(t.contains("Testes") && t.contains("Detalhes"), "{t}");
        // ferramentas em pouca altura: um painel por vez, com indicação de como trocar
        app.tab = Tab::Ferramentas;
        let t = text_of(&app, 60, 12);
        assert!(t.contains("1/3"), "{t}");
        app.tool_panel = 1;
        assert!(text_of(&app, 60, 12).contains("2/3"));
        // só o mínimo absoluto mostra o aviso
        let t = text_of(&app, 20, 4);
        assert!(t.contains("Janela pequena") && t.contains("24x6"), "{t}");
    }

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    fn wheel(down: bool) -> MouseEvent {
        MouseEvent { kind: if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp }, column: 5, row: 5, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn mouse_clicks_tabs_lists_and_game_buttons() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        let _ = text_of(&app, 130, 40);
        // clicar no nome de uma aba troca de aba
        let (r, t) = app.hits.borrow().tabs.iter().find(|(_, t)| *t == Tab::Testes).copied().unwrap();
        app.on_mouse(click(r.x + 2, r.y));
        assert_eq!(app.tab, t);
        // clicar num item da lista de testes seleciona; duplo clique inicia (aqui vira "já há teste" com um falso em andamento)
        let _ = text_of(&app, 130, 40);
        let (lr, ..) = app.hits.borrow().lists.iter().find(|(_, id, ..)| *id == ListId::Jobs).copied().unwrap();
        app.on_mouse(click(lr.x + 3, lr.y + 2));
        assert_eq!(app.job_sel, 2);
        // clique fora dos itens (abaixo do último) não muda nada
        app.on_mouse(click(lr.x + 3, lr.y + lr.height - 1));
        assert!(app.job_sel < jobs::ALL.len());
        // roda do mouse move a seleção da lista
        app.on_mouse(wheel(false));
        assert_eq!(app.job_sel, 1);
        // jogos: botão clicável liga o GameMode
        put(&d.path().join("cfg/x"), "");
        app.env.store.update(|s| s.custom_games.push(CustomGame { name: "Jogo".into(), exe: "/bin/true".into(), args: String::new() })).unwrap();
        app.reload_games();
        app.goto(Tab::Jogos);
        let _ = text_of(&app, 130, 40);
        let (br, c) = app.hits.borrow().buttons.iter().find(|(_, c)| *c == 'g').copied().unwrap();
        let before = app.profile_of(&app.games[0].clone()).gamemode;
        app.on_mouse(click(br.x + 1, br.y));
        assert_eq!(c, 'g');
        assert_ne!(app.profile_of(&app.games[0].clone()).gamemode, before);
        // clicar fora de qualquer área é ignorado
        assert!(matches!(app.on_mouse(click(0, 39)), Effect::None));
    }

    #[test]
    fn double_click_on_a_game_launches_but_on_a_tweak_it_only_selects() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        app.env.store.update(|s| s.custom_games.push(CustomGame { name: "Jogo".into(), exe: "/bin/true".into(), args: String::new() })).unwrap();
        app.reload_games();
        app.goto(Tab::Jogos);
        let _ = text_of(&app, 130, 40);
        let (lr, ..) = app.hits.borrow().lists.iter().find(|(_, id, ..)| *id == ListId::Games).copied().unwrap();
        assert!(matches!(app.on_mouse(click(lr.x + 2, lr.y)), Effect::None), "um clique só seleciona");
        assert!(matches!(app.on_mouse(click(lr.x + 2, lr.y)), Effect::Launch(_)), "duplo clique inicia");
        app.goto(Tab::Ferramentas);
        let _ = text_of(&app, 130, 40);
        let (tr, ..) = app.hits.borrow().lists.iter().find(|(_, id, ..)| matches!(id, ListId::Tool(0))).copied().unwrap();
        app.status = None;
        for _ in 0..2 {
            app.on_mouse(click(tr.x + 2, tr.y));
        }
        assert!(app.status.is_none(), "duplo clique em ajuste não aplica nada");
    }

    #[test]
    fn help_opens_with_question_mark_and_closes_with_any_key() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        app.on_key(key(KeyCode::Char('?')));
        assert!(app.help);
        let t = text_of(&app, 100, 30);
        assert!(t.contains("Ajuda") && t.contains("Duplo clique"), "{t}");
        // com a ajuda aberta, 'q' só fecha a ajuda
        assert!(matches!(app.on_key(key(KeyCode::Char('q'))), Effect::None) && !app.help && !app.quit);
        app.on_key(key(KeyCode::F(1)));
        assert!(app.help);
        app.on_mouse(click(1, 1));
        assert!(!app.help);
        app.on_key(key(KeyCode::F(1)));
        assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Effect::Quit));
        // ajuda cabe em janela pequena sem estourar
        let mut app = app_with_data(d.path());
        app.help = true;
        let _ = text_of(&app, 30, 8);
    }

    #[test]
    fn processes_tab_lists_and_sorts() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        app.goto(Tab::Processos);
        let live = app.live.as_mut().unwrap();
        live.procs = vec![
            crate::procs::Proc { pid: 1, name: "leve".into(), cpu: 1.0, rss: 900 << 20 },
            crate::procs::Proc { pid: 2, name: "pesado".into(), cpu: 80.0, rss: 10 << 20 },
        ];
        let t = text_of(&app, 100, 20);
        assert!(t.find("pesado").unwrap() < t.find("leve").unwrap(), "{t}");
        app.on_key(key(KeyCode::Char('m')));
        let t = text_of(&app, 100, 20);
        assert!(t.find("leve").unwrap() < t.find("pesado").unwrap(), "{t}");
        assert!(t.contains("memória"));
    }

    #[test]
    fn narrow_terminals_keep_scrolling_inside_the_visible_area() {
        let d = tempfile::tempdir().unwrap();
        let mut app = app_with_data(d.path());
        app.tab = Tab::Sensores;
        let _ = text_of(&app, 50, 12);
        let small = app.max_scroll.get();
        let _ = text_of(&app, 120, 60);
        assert!(app.max_scroll.get() <= small, "janela maior rola menos");
    }

    #[test]
    fn tab_navigation_wraps_and_digits_jump() {
        let d = tempfile::tempdir().unwrap();
        let mut app = App::new(test_env(d.path()));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Sensores);
        app.on_key(key(KeyCode::BackTab));
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Processos, "volta do início para o fim");
        app.on_key(key(KeyCode::Char('6')));
        assert_eq!(app.tab, Tab::Testes);
        app.on_key(key(KeyCode::Char('0')));
        assert_eq!(app.tab, Tab::Testes, "0 não existe");
        app.on_key(key(KeyCode::Char('9')));
        assert_eq!(app.tab, Tab::Processos);
        app.on_key(key(KeyCode::Char('6')));
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
