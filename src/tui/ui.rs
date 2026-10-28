//! Desenho das telas. Só lê o estado (`App`); nunca coleta dados nem executa nada.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Tabs, Wrap};

use super::jobs::{self, Running};
use super::theme::Theme;
use super::{App, Level, ListId, TABS, Tab};
use crate::launch;
use crate::sensors::SensorKind;
use crate::util::{fmt_bytes, fmt_duration, fmt_mhz, trunc};

/// Barra de texto de largura fixa: `█████░░░░░`.
pub fn bar(pct: f64, width: usize) -> String {
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled.min(width)), "░".repeat(width - filled.min(width)))
}

/// Um caractere de altura proporcional (▁ a █) para mostrar o uso de cada núcleo numa linha só.
fn level_glyph(pct: f64) -> char {
    const G: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    G[((pct.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize]
}

fn heading(th: &Theme, t: &str) -> Line<'static> {
    Line::from(Span::styled(t.to_string(), th.heading()))
}

fn kv(th: &Theme, k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k:<14}"), th.muted()), Span::raw(v.into())])
}

fn opt(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.0}{unit}")).unwrap_or_else(|| "—".into())
}

fn waiting(th: &Theme) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled("Coletando dados...", th.muted()))]
}

/// Quanto espaço o terminal oferece. A interface se adapta sozinha: painéis lado a lado passam a ficar
/// empilhados, bordas e cabeçalho encolhem e a barra de atalhos fica mais curta. (O tamanho da letra é do
/// terminal e o programa não consegue mudá-lo; o que ele ajusta é o que desenha.)
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Dense {
    /// menos de 80 colunas: painéis lado a lado ficam um sobre o outro
    pub narrow: bool,
    /// menos de 24 linhas: cabeçalho e rodapé de uma linha só
    pub short: bool,
    /// bem pequeno: sem bordas, só o título de cada painel
    pub tiny: bool,
}

pub fn dense(a: Rect) -> Dense {
    Dense { narrow: a.width < 80, short: a.height < 24, tiny: a.width < 64 || a.height < 16 }
}

/// Abaixo disto nem o modo mínimo cabe.
pub const MIN_W: u16 = 24;
pub const MIN_H: u16 = 6;

fn blk<'a>(app: &App, title: impl Into<Line<'a>>) -> Block<'a> {
    fblk(app, title, false)
}

fn fblk<'a>(app: &App, title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
    let th = &app.theme;
    if app.dense.get().tiny {
        let l: Line = title.into();
        Block::new().title(l.style(if focused { th.selection() } else { th.heading() }))
    } else {
        th.focus_block(title, focused)
    }
}

/// Divide a área em dois painéis: lado a lado, ou um sobre o outro quando o terminal é estreito.
/// `first_pct` vale para o lado a lado; empilhado, o primeiro ocupa `first_rows` linhas (no máximo metade).
fn split2(d: Dense, area: Rect, first_pct: u16, first_rows: u16) -> [Rect; 2] {
    if d.narrow {
        Layout::vertical([Constraint::Length(first_rows.min(area.height / 2).max(3)), Constraint::Min(2)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(first_pct), Constraint::Percentage(100 - first_pct)]).areas(area)
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let th = &app.theme;
    let area = f.area();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = format!("Janela pequena ({}x{}).\nMínimo {MIN_W}x{MIN_H}. (q sai)", area.width, area.height);
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }), area);
        return;
    }
    *app.hits.borrow_mut() = Default::default();
    let d = dense(area);
    app.dense.set(d);
    let compact = d.short || d.tiny;
    let (top_h, foot_h) = if compact { (1, 1) } else { (3, 2) };
    let [top, body, foot] = Layout::vertical([Constraint::Length(top_h), Constraint::Min(1), Constraint::Length(foot_h)]).areas(area);
    // abas: com pouco espaço só a ativa mostra o nome, as outras viram só o número
    let full_names = area.width >= 120;
    let titles: Vec<Line> = TABS.iter().enumerate().map(|(i, t)| if full_names || i == app.tab.index() { Line::from(format!("{} {}", i + 1, t.title())) } else { Line::from((i + 1).to_string()) }).collect();
    let mut tabs = Tabs::new(titles).select(app.tab.index()).highlight_style(th.selection()).divider(th.divider.as_str());
    let mut tab_area = top;
    if !compact {
        let b = th.block(" PINGU ").title_top(Line::from(Span::styled(format!(" v{} · ? ajuda ", env!("CARGO_PKG_VERSION")), th.muted())).right_aligned());
        tab_area = b.inner(top);
        tabs = tabs.block(b);
    }
    f.render_widget(tabs, top);
    // áreas clicáveis das abas: cada uma tem 1 espaço de cada lado do nome e o divisor entre elas
    let mut x = tab_area.x;
    let right = tab_area.x + tab_area.width;
    for (i, t) in TABS.iter().enumerate() {
        let w = if full_names || i == app.tab.index() { format!("{} {}", i + 1, t.title()).chars().count() } else { (i + 1).to_string().len() } as u16 + 2;
        if x >= right {
            break;
        }
        app.hits.borrow_mut().tabs.push((Rect::new(x, tab_area.y, w.min(right - x), 1), *t));
        x += w + th.divider.chars().count() as u16;
    }
    match app.tab {
        Tab::Resumo => resumo(f, app, body),
        Tab::Sensores => scrolled(f, app, body, "Sensores", sensor_lines(app)),
        Tab::Discos => scrolled(f, app, body, "Discos e volumes", disk_lines(app)),
        Tab::Gpu => scrolled(f, app, body, "Placas de vídeo", gpu_lines(app)),
        Tab::Rede => scrolled(f, app, body, "Energia, rede e portas seriais", net_lines(app)),
        Tab::Testes => tests(f, app, body),
        Tab::Jogos => games(f, app, body),
        Tab::Ferramentas => tools(f, app, body),
        Tab::Processos => {
            let by = app.proc_sort;
            let title = format!("Processos (por {} · c CPU · m memória)", if by == crate::procs::SortBy::Cpu { "CPU" } else { "memória" });
            scrolled(f, app, body, &title, proc_lines(app))
        }
    }
    footer(f, app, foot);
    if app.help {
        help(f, app, area);
    }
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let short = area.width < 80;
    let hint = match (app.tab, short) {
        (Tab::Processos, false) => "↑↓ PgUp/PgDn rolar · c ordenar por CPU · m por memória · Tab/1-9 abas · ? ajuda · q sai",
        (Tab::Processos, true) => "↑↓ · c CPU · m mem · ? · q",
        (Tab::Testes, false) => "↑↓ escolher · Enter iniciar · Esc cancela · Tab/1-9 abas · ? ajuda · q sai",
        (Tab::Testes, true) => "↑↓ · Enter · Esc · Tab · q",
        (Tab::Jogos, false) => "↑↓ · g gamemode · m mangohud · d dGPU · p perf. · i prioridade · Enter iniciar · ? ajuda · q sai",
        (Tab::Jogos, true) => "↑↓ · g m d p i · Enter · Tab · q",
        (Tab::Ferramentas, false) => "←→ painel · ↑↓ · Enter aplica/desfaz/instala · r atualizar · Tab/1-9 abas · ? ajuda · q sai",
        (Tab::Ferramentas, true) => "←→ painel · ↑↓ · Enter · Tab · q",
        (_, false) => "↑↓ PgUp/PgDn rolar · ←→ ou Tab/1-9 trocar de aba · t tema · ? ajuda · q sai",
        (_, true) => "↑↓ rolar · ←→ aba · ? ajuda · q",
    };
    let status = app.status.as_ref().map(|(lv, t)| (match lv { Level::Info => th.accent, Level::Ok => th.ok, Level::Err => th.err }, t));
    if area.height < 2 {
        // uma linha só: a mensagem de status tem prioridade sobre os atalhos
        match status {
            Some((c, t)) => f.render_widget(Paragraph::new(Span::styled(trunc(t, area.width as usize), Style::new().fg(c))), area),
            None => f.render_widget(Paragraph::new(Span::styled(trunc(hint, area.width as usize), th.muted())), area),
        }
        return;
    }
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    if let Some((c, t)) = status {
        f.render_widget(Paragraph::new(Span::styled(trunc(t, area.width as usize), Style::new().fg(c))), l1);
    }
    f.render_widget(Paragraph::new(Span::styled(trunc(hint, area.width as usize), th.muted())), l2);
}

/// Texto com rolagem; calcula a rolagem máxima para o teclado não passar do fim.
fn scrolled(f: &mut Frame, app: &App, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let th = &app.theme;
    let probe = blk(app, "");
    let view = probe.inner(area).height;
    let max = (lines.len().min(u16::MAX as usize) as u16).saturating_sub(view);
    app.max_scroll.set(max);
    let y = app.scroll[app.tab.index()].min(max);
    let shown = if max > 0 { format!(" {title} ({}/{}) ", y + 1, max + 1) } else { format!(" {title} ") };
    let _ = th;
    f.render_widget(Paragraph::new(lines).block(blk(app, shown)).scroll((y, 0)), area);
}

// ---------------------------------------------------------------------------------------------
// Resumo
// ---------------------------------------------------------------------------------------------

pub fn system_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let Some(s) = &app.stat else { return waiting(th) };
    let mut v = vec![kv(th, "Sistema", format!("{} · {}", s.os.pretty_name, s.os.arch)), kv(th, "Kernel", s.os.kernel.clone()), kv(th, "Máquina", s.os.hostname.clone())];
    v.push(kv(th, "Ligado há", fmt_duration(s.os.uptime_secs)));
    if let Some(vm) = &s.os.virtualization {
        v.push(kv(th, "Virtualização", vm.clone()));
    }
    let board = format!("{} {}", s.board.board_vendor, s.board.board_name);
    if !board.trim().is_empty() {
        v.push(kv(th, "Placa-mãe", board.trim().to_string()));
    }
    v.push(kv(th, "CPU", s.cpu.model.clone()));
    v.push(kv(th, "Núcleos", format!("{} / {} threads", s.cpu.cores, s.cpu.threads)));
    if let Some(l) = &app.live {
        v.push(kv(th, "Memória", format!("{} total · {} livre", fmt_bytes(l.mem.total), fmt_bytes(l.mem.available))));
        for g in &l.gpus {
            v.push(kv(th, "GPU", g.name.clone()));
        }
    }
    for d in &s.displays {
        if let Some(e) = &d.edid {
            let size = e.diagonal_inches().map(|i| format!(" {i:.0}\""));
            let res = e.preferred.map(|(w, h, hz)| format!(" {w}x{h}@{hz:.0}Hz"));
            v.push(kv(th, "Tela", format!("{}{}{} ({})", e.name.clone().unwrap_or_else(|| e.manufacturer.clone()), size.unwrap_or_default(), res.unwrap_or_default(), d.connector)));
        }
    }
    v
}

fn resumo(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let d = app.dense.get();
    // estreito: "Agora" (medidores) em cima, porque é o que muda; "Sistema" ocupa o resto
    let (left, right) = if d.narrow {
        let [r, l] = split2(d, area, 50, 7);
        (l, r)
    } else {
        let [l, r] = split2(d, area, 50, 0);
        (l, r)
    };
    f.render_widget(Paragraph::new(system_lines(app)).block(blk(app, " Sistema ")).wrap(Wrap { trim: true }), left);
    let block = blk(app, " Agora ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let Some(l) = &app.live else {
        f.render_widget(Paragraph::new(waiting(th)), inner);
        return;
    };
    let [g1, g2, g3, spark, rest] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(if inner.height >= 10 { 4 } else { 0 }), Constraint::Min(0)]).areas(inner);
    gauge(f, th, g1, "CPU ", l.cpu.total, format!("{:.0}%", l.cpu.total));
    let used = l.mem.total.saturating_sub(l.mem.available);
    let mem_pct = if l.mem.total > 0 { used as f64 / l.mem.total as f64 * 100.0 } else { 0.0 };
    gauge(f, th, g2, "RAM ", mem_pct, format!("{} / {}", fmt_bytes(used), fmt_bytes(l.mem.total)));
    let swap_used = l.mem.swap_total.saturating_sub(l.mem.swap_free);
    let swap_pct = if l.mem.swap_total > 0 { swap_used as f64 / l.mem.swap_total as f64 * 100.0 } else { 0.0 };
    gauge(f, th, g3, "Swap", swap_pct, if l.mem.swap_total == 0 { "sem swap".into() } else { format!("{} / {}", fmt_bytes(swap_used), fmt_bytes(l.mem.swap_total)) });
    let cpu_spark = Sparkline::default().data(&app.cpu_history).max(100).style(th.accent_style()).block(Block::new().borders(Borders::TOP).title(" CPU (últimos minutos) "));
    if spark.width >= 50 {
        let [a, b] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(spark);
        f.render_widget(cpu_spark, a);
        f.render_widget(Sparkline::default().data(&app.mem_history).max(100).style(Style::new().fg(th.ok)).block(Block::new().borders(Borders::TOP).title(" RAM ")), b);
    } else {
        f.render_widget(cpu_spark, spark);
    }
    let mut lines = vec![];
    if !l.cpu.per_core.is_empty() {
        let mut spans = vec![Span::styled(format!("{:<14}", "Núcleos"), th.muted())];
        spans.extend(l.cpu.per_core.iter().take(inner.width.saturating_sub(16) as usize).map(|c| Span::styled(level_glyph(*c).to_string(), Style::new().fg(th.level_color(*c)))));
        lines.push(Line::from(spans));
    }
    if let Some(s) = &app.stat {
        lines.push(kv(th, "Carga média", s.os.load_avg.map(|(a, b, c)| format!("{a:.2} {b:.2} {c:.2}")).unwrap_or_else(|| "—".into())));
    }
    if let Some(t) = crate::sensors::cpu_temperature(&l.sensors) {
        lines.push(Line::from(vec![Span::styled(format!("{:<14}", "Temp. CPU"), th.muted()), Span::styled(format!("{t:.0} °C"), Style::new().fg(th.temp_color(t)))]));
    }
    for g in &l.gpus {
        lines.push(kv(th, "GPU", format!("{} · uso {} · temp {} · {}", trunc(&g.name, 20), opt(g.util_gpu, "%"), opt(g.temp, " °C"), opt(g.power_w, " W"))));
    }
    if let Some(b) = l.power.batteries.first() {
        lines.push(kv(th, "Bateria", format!("{} · {}", opt(b.capacity, "%"), b.status)));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("Sem sensores disponíveis (comum em máquina virtual).", th.muted())));
    }
    f.render_widget(Paragraph::new(lines), rest);
}

fn gauge(f: &mut Frame, th: &Theme, area: Rect, name: &str, pct: f64, label: String) {
    let [n, g] = Layout::horizontal([Constraint::Length(5), Constraint::Min(5)]).areas(area);
    f.render_widget(Paragraph::new(name.to_string()), n);
    f.render_widget(Gauge::default().ratio((pct / 100.0).clamp(0.0, 1.0)).label(label).gauge_style(th.gauge(th.level_color(pct))), g);
}

// ---------------------------------------------------------------------------------------------
// Textos
// ---------------------------------------------------------------------------------------------

pub fn sensor_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let Some(l) = &app.live else { return waiting(th) };
    if l.sensors.is_empty() {
        return vec![Line::from("Nenhum sensor encontrado (máquina virtual ou sem drivers hwmon)."), Line::from(Span::styled("Dica: instale e configure o lm-sensors (aba Ferramentas).", th.muted()))];
    }
    let mut v = vec![];
    let mut last = String::new();
    for s in &l.sensors {
        if s.chip_display != last {
            if !last.is_empty() {
                v.push(Line::from(""));
            }
            v.push(heading(th, &s.chip_display));
            last = s.chip_display.clone();
        }
        let (val, color) = match s.kind {
            SensorKind::Temp => (format!("{:.1} °C", s.value), th.temp_color(s.value)),
            SensorKind::Fan => (format!("{:.0} RPM", s.value), Color::Reset),
            SensorKind::Voltage => (format!("{:.3} V", s.value), Color::Reset),
            SensorKind::Power => (format!("{:.1} W", s.value), Color::Reset),
            SensorKind::Current => (format!("{:.2} A", s.value), Color::Reset),
            SensorKind::Freq => (fmt_mhz(s.value / 1e6), Color::Reset),
            SensorKind::Pwm => (format!("{:.0}%", s.value / 255.0 * 100.0), Color::Reset),
        };
        v.push(Line::from(vec![Span::raw(format!("  {:<26}", trunc(&s.label, 26))), Span::styled(val, Style::new().fg(color))]));
    }
    v
}

pub fn disk_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let mut v = vec![];
    match &app.stat {
        None => return waiting(th),
        Some(s) => {
            v.push(heading(th, "Discos"));
            for d in &s.disks {
                v.push(Line::from(format!("{} · {} · {} · {} · {}", d.name, d.title(), fmt_bytes(d.size), d.kind(), d.bus)));
                v.push(Line::from(Span::styled(format!("    temp {} · {}", opt(d.temp, " °C"), d.verdict.label()), th.muted())));
            }
            if s.disks.is_empty() {
                v.push(Line::from("Nenhum disco encontrado."));
            }
        }
    }
    if let Some(l) = &app.live {
        v.push(Line::from(""));
        v.push(heading(th, "Volumes montados"));
        for m in &l.mounts {
            let pct = if m.total > 0 { m.used as f64 / m.total as f64 * 100.0 } else { 0.0 };
            v.push(Line::from(vec![Span::raw(format!("{:<26} {:<7} ", trunc(&m.mountpoint, 26), m.fstype)), Span::styled(bar(pct, 12), Style::new().fg(th.level_color(pct))), Span::styled(format!(" {:>3.0}%", pct), Style::new().fg(th.level_color(pct))), Span::raw(format!("  {} de {}", fmt_bytes(m.used), fmt_bytes(m.total)))]));
        }
    }
    v.push(Line::from(""));
    v.push(Line::from(Span::styled("SMART detalhado: `pingu disks --smart` (precisa de sudo).", th.muted())));
    v
}

pub fn gpu_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let Some(l) = &app.live else { return waiting(th) };
    if l.gpus.is_empty() {
        return vec![Line::from("Nenhuma GPU encontrada.")];
    }
    let mut v = vec![];
    for g in &l.gpus {
        v.push(heading(th, &format!("[{}] {} ({})", g.index, g.name, g.vendor.name())));
        v.push(kv(th, "Driver", g.driver_version.clone().or(g.driver.clone()).unwrap_or_else(|| "—".into())));
        v.push(kv(th, "VRAM", format!("{} / {}", g.vram_used.map(fmt_bytes).unwrap_or_else(|| "—".into()), g.vram_total.map(fmt_bytes).unwrap_or_else(|| "—".into()))));
        v.push(kv(th, "Uso", opt(g.util_gpu, "%")));
        v.push(kv(th, "Temperatura", opt(g.temp, " °C")));
        v.push(kv(th, "Consumo", format!("{} (limite {})", opt(g.power_w, " W"), opt(g.power_limit_w, " W"))));
        v.push(kv(th, "Clock", format!("núcleo {} · memória {}", opt(g.clock_core, " MHz"), opt(g.clock_mem, " MHz"))));
        v.push(Line::from(""));
    }
    v
}

pub fn net_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let mut v = vec![heading(th, "Energia")];
    match &app.live {
        None => return waiting(th),
        Some(l) => {
            v.push(kv(th, "Tomada", match l.power.on_ac { Some(true) => "ligada", Some(false) => "desligada (na bateria)", None => "—" }));
            if l.power.batteries.is_empty() {
                v.push(Line::from("Sem bateria (desktop)."));
            }
            for b in &l.power.batteries {
                v.push(kv(th, &b.name, format!("{} · {} · saúde {} · {} ciclos", opt(b.capacity, "%"), b.status, opt(b.health, "%"), b.cycles.map(|c| c.to_string()).unwrap_or_else(|| "—".into()))));
            }
            let c = &l.power.cpu;
            v.push(kv(th, "Governador", match &c.governor {
                Some(g) => format!("{g} ({})", c.governors.join("/")),
                None => "não exposto por este sistema".into(),
            }));
            v.push(kv(th, "Boost", c.boost.map(|b| if b { "ligado" } else { "desligado" }).unwrap_or("—")));
            v.push(kv(th, "Perfil", c.platform_profile.clone().unwrap_or_else(|| "—".into())));
        }
    }
    if let Some(s) = &app.stat {
        v.push(Line::from(""));
        v.push(heading(th, "Rede"));
        for i in &s.interfaces {
            v.push(Line::from(format!("{:<12} {:<9} {:<8} {} {}", i.name, i.kind, if i.up { "ativa" } else { "inativa" }, i.speed_mbps.map(|s| format!("{s} Mb/s")).unwrap_or_default(), i.driver.clone().unwrap_or_default())));
        }
        v.push(kv(th, "DNS", if s.dns.is_empty() { "—".to_string() } else { s.dns.join(", ") }));
        v.push(Line::from(Span::styled("Latência, velocidade e bufferbloat: `pingu net --test`.", th.muted())));
        v.push(Line::from(""));
        v.push(heading(th, "Portas seriais USB"));
        for p in &s.ports {
            v.push(Line::from(format!("{} · {} {} · {}{}", p.device, p.vendor_id.clone().unwrap_or_default(), p.product_id.clone().unwrap_or_default(), p.product.clone().unwrap_or_default(), if p.accessible { "" } else { " · SEM ACESSO" })));
        }
        for a in &s.serial_advice {
            v.push(Line::from(Span::styled(format!("! {}", a.problem), th.warn())));
            v.push(Line::from(format!("  → {}", a.fix)));
        }
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------------------------

fn list_state(sel: usize) -> ListState {
    ListState::default().with_selected(Some(sel))
}

fn tests(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let d = app.dense.get();
    let [left, right] = split2(d, area, 40, jobs::ALL.len() as u16 + 2);
    let items: Vec<ListItem> = jobs::ALL.iter().map(|k| ListItem::new(k.title())).collect();
    let list = List::new(items).block(blk(app, " Testes ")).highlight_style(th.selection()).highlight_symbol(th.pointer.as_str());
    let mut st = list_state(app.job_sel);
    f.render_stateful_widget(list, left, &mut st);
    app.hits.borrow_mut().lists.push((blk(app, "").inner(left), ListId::Jobs, st.offset(), jobs::ALL.len()));

    let block = blk(app, " Detalhes ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let kind = jobs::ALL[app.job_sel.min(jobs::ALL.len() - 1)];
    let [desc, prog, res] = Layout::vertical([Constraint::Length(if d.narrow { 5 } else { 4 }), Constraint::Length(3), Constraint::Min(0)]).areas(inner);
    let mut d = vec![Line::from(kind.desc())];
    if kind.heavy() {
        d.push(Line::from(Span::styled("Carga pesada: o PC pode esquentar e as ventoinhas vão subir.", th.warn())));
    }
    f.render_widget(Paragraph::new(d).wrap(Wrap { trim: true }), desc);
    if let Some(j) = &app.job {
        running(f, th, j, prog);
    } else {
        f.render_widget(Paragraph::new(Span::styled("Enter inicia o teste selecionado.", th.muted())), prog);
    }
    let mut lines = vec![];
    if let Some((k, done)) = &app.job_result {
        lines.push(heading(th, &format!("Resultado: {}", k.title())));
        lines.extend(done.lines.iter().map(|l| Line::from(l.clone())));
        if done.cancelled {
            lines.push(Line::from(Span::styled("Cancelado: resultado parcial, não foi salvo.", th.warn())));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), res);
}

fn running(f: &mut Frame, th: &Theme, j: &Running, area: Rect) {
    let [a, b] = Layout::vertical([Constraint::Length(1), Constraint::Length(2)]).areas(area);
    f.render_widget(Paragraph::new(Span::styled(format!("Executando: {} · {}", j.kind.title(), fmt_duration(j.started.elapsed().as_secs())), th.ok())), a);
    f.render_widget(Gauge::default().ratio(j.fraction().clamp(0.0, 1.0)).label(trunc(&j.label(), area.width as usize)).gauge_style(th.gauge(th.ok)), b);
}

// ---------------------------------------------------------------------------------------------
// Jogos
// ---------------------------------------------------------------------------------------------



fn games(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let d = app.dense.get();
    let [left, right] = split2(d, area, 45, app.games.len().min(5) as u16 + 2);
    if app.games.is_empty() {
        f.render_widget(Paragraph::new(vec![Line::from("Nenhum jogo encontrado (Steam, Heroic, Lutris)."), Line::from(""), Line::from("Adicione um manualmente com:"), Line::from("  pingu games add \"Nome\" /caminho/do/executavel")]).block(blk(app, " Jogos ")).wrap(Wrap { trim: true }), area);
        return;
    }
    let items: Vec<ListItem> = app.games.iter().map(|g| ListItem::new(format!("{} · {}", trunc(&g.name, 30), g.source.name()))).collect();
    let list = List::new(items).block(blk(app, format!(" Jogos ({}) ", app.games.len()))).highlight_style(th.selection()).highlight_symbol(th.pointer.as_str());
    let mut st = list_state(app.game_sel);
    f.render_stateful_widget(list, left, &mut st);
    app.hits.borrow_mut().lists.push((blk(app, "").inner(left), ListId::Games, st.offset(), app.games.len()));
    let Some(g) = app.games.get(app.game_sel) else { return };
    let p = app.profile_of(g);
    let inner = blk(app, "").inner(right);
    let pasta = trunc(&g.install_dir.clone().unwrap_or_else(|| "—".into()), (inner.width as usize).saturating_sub(15).max(8));
    let mut v = vec![heading(th, &trunc(&g.name, inner.width as usize)), kv(th, "Origem", g.source.name()), kv(th, "Pasta", pasta), Line::from("")];
    // um botão por linha: clicar equivale a apertar a tecla indicada
    let buttons = [
        ('g', format!("{} GameMode (g)", th.check(p.gamemode))),
        ('m', format!("{} MangoHud (m)", th.check(p.mangohud))),
        ('d', format!("{} GPU dedicada (d)", th.check(p.dgpu))),
        ('p', format!("{} Governador performance (p)", th.check(p.performance_governor))),
        ('i', format!("Prioridade (i): {}", p.priority)),
    ];
    for (c, label) in buttons {
        let y = inner.y + v.len() as u16;
        if y < inner.y + inner.height {
            app.hits.borrow_mut().buttons.push((Rect::new(inner.x, y, (label.chars().count() as u16).min(inner.width), 1), c));
        }
        v.push(Line::from(label));
    }
    v.push(Line::from(""));
    match launch::plan(&p, app.live.as_ref().and_then(|l| crate::gpu::primary(&l.gpus)).map(|g| g.vendor), crate::util::is_root(), &crate::util::have) {
        Ok(pl) if g.steam_appid.is_some() => {
            v.push(heading(th, "Opções de inicialização da Steam"));
            v.push(Line::from(launch::steam_options(&pl)));
            v.extend(pl.warnings.iter().map(|w| Line::from(Span::styled(format!("aviso: {w}"), th.warn()))));
        }
        Ok(pl) => {
            v.push(Line::from(Span::styled("Enter inicia o jogo com este perfil.", th.ok())));
            v.extend(pl.warnings.iter().map(|w| Line::from(Span::styled(format!("aviso: {w}"), th.warn()))));
        }
        Err(e) => v.push(Line::from(Span::styled(format!("perfil inválido: {e}"), th.err()))),
    }
    f.render_widget(Paragraph::new(v).block(blk(app, " Perfil ")).wrap(Wrap { trim: false }), right);
}

// ---------------------------------------------------------------------------------------------
// Ferramentas
// ---------------------------------------------------------------------------------------------

fn tools(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let d = app.dense.get();
    // pouca altura: mostra só o painel em foco (←→ troca) em vez de três painéis minúsculos
    let single = area.height < 18;
    let panels = if single { [area; 3].to_vec() } else { Layout::vertical([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)]).split(area).to_vec() };
    let full = [" Ajustes de sistema (valem até reiniciar) ", " Programas de inicialização ", " Instalar utilitários "];
    let short = [" Ajustes ", " Inicialização ", " Utilitários "];
    let names = if d.narrow || single { short } else { full };
    let single_titles: Vec<String> = names.iter().enumerate().map(|(i, n)| format!("{}[{}/3 ←→] ", n.trim_end(), i + 1)).collect();
    let titles: Vec<&str> = if single { single_titles.iter().map(|s| s.as_str()).collect() } else { names.to_vec() };
    let lens = [app.tweaks.len(), app.startup.len(), app.apps.len()];
    let rows: [Vec<ListItem>; 3] = [
        app.tweaks.iter().map(|t| ListItem::new(format!("{} {:<20} {} → {}  {}", th.check(t.applied), t.id, t.current.clone().unwrap_or_else(|| "—".into()), t.recommended, if !t.available { "(indisponível)" } else if t.can_restore { "(restaurável)" } else { "" }))).collect(),
        app.startup.iter().map(|e| ListItem::new(format!("{} {:<30} {}", th.check(e.enabled), trunc(&e.name, 30), if e.system { "(sistema)" } else { "" }))).collect(),
        app.apps.iter().map(|a| ListItem::new(format!("{} {:<14} {}", th.check(a.installed), a.id, a.title))).collect(),
    ];
    for (i, items) in rows.into_iter().enumerate() {
        let focused = app.tool_panel == i;
        if single && !focused {
            continue;
        }
        let empty = items.is_empty();
        let blank = " ".repeat(th.pointer.chars().count());
        let list = List::new(items).block(fblk(app, titles[i], focused)).highlight_style(if focused { th.selection() } else { Style::new() }).highlight_symbol(if focused { th.pointer.as_str() } else { blank.as_str() });
        if empty {
            f.render_widget(Paragraph::new("(vazio)").block(fblk(app, titles[i], focused)), panels[i]);
        } else {
            let mut st = list_state(app.tool_sel[i]);
            f.render_stateful_widget(list, panels[i], &mut st);
            app.hits.borrow_mut().lists.push((fblk(app, "", focused).inner(panels[i]), ListId::Tool(i), st.offset(), lens[i]));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Processos e ajuda
// ---------------------------------------------------------------------------------------------

pub fn proc_lines(app: &App) -> Vec<Line<'static>> {
    let th = &app.theme;
    let Some(l) = &app.live else { return waiting(th) };
    if l.procs.is_empty() {
        return vec![Line::from("Não consegui ler /proc (sem processos para mostrar).")];
    }
    let mut v = vec![Line::from(Span::styled(format!("{:>7} {:>7} {:>11}  {}", "PID", "CPU", "Memória", "Nome"), th.heading()))];
    for p in crate::procs::top(l.procs.clone(), app.proc_sort, 200) {
        v.push(Line::from(vec![
            Span::raw(format!("{:>7} ", p.pid)),
            Span::styled(format!("{:>6.1}%", p.cpu), Style::new().fg(th.level_color(p.cpu.min(100.0)))),
            Span::raw(format!(" {:>11}  {}", fmt_bytes(p.rss), p.name)),
        ]));
    }
    v
}

fn help(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let key = |k: &str, t: &str| Line::from(vec![Span::styled(format!("{k:<16}"), th.accent_style()), Span::raw(t.to_string())]);
    let lines = vec![
        heading(th, "Em qualquer aba"),
        key("Tab / Shift+Tab", "próxima / anterior aba (ou ←→ e os números 1-9)"),
        key("↑↓ PgUp PgDn", "rolar ou escolher; Home/End vão ao início/fim"),
        key("t / T", "t troca de tema (temporário); T recarrega o theme.json"),
        key("? / F1", "esta ajuda; Esc ou qualquer tecla fecha"),
        key("q / Ctrl+C", "sair (Ctrl+C e Esc cancelam um teste em andamento)"),
        Line::from(""),
        heading(th, "Por aba"),
        key("Testes", "Enter inicia · Esc cancela"),
        key("Jogos", "g GameMode · m MangoHud · d GPU dedicada · p governador · i prioridade · Enter inicia"),
        key("Ferramentas", "←→ muda de painel · Enter aplica/desfaz/instala · r atualiza"),
        key("Processos", "c ordena por CPU · m ordena por memória"),
        Line::from(""),
        heading(th, "Mouse"),
        key("Clique", "abas, itens das listas e os botões dos jogos"),
        key("Duplo clique", "inicia o teste ou o jogo (como Enter)"),
        key("Roda", "rola o texto ou muda a seleção"),
        Line::from(""),
        Line::from(Span::styled("A fonte e o tamanho da letra são do seu terminal (Ctrl +/-).", th.muted())),
    ];
    let w = area.width.saturating_sub(4).min(86);
    let h = (lines.len() as u16 + 2).min(area.height);
    let r = Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h);
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(th.focus_block(" Ajuda ", true)).wrap(Wrap { trim: true }), r);
}
