//! Desenho das telas. Só lê o estado (`App`); nunca coleta dados nem executa nada.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Tabs, Wrap};

use super::jobs::{self, Running};
use super::{App, Level, TABS, Tab};
use crate::launch;
use crate::sensors::SensorKind;
use crate::util::{fmt_bytes, fmt_duration, fmt_mhz, trunc};

const MUTED: Color = Color::DarkGray;
const ACCENT: Color = Color::Cyan;

fn heading(t: &str) -> Line<'static> {
    Line::from(Span::styled(t.to_string(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

fn kv(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{k:<14}"), Style::new().fg(MUTED)), Span::raw(v.into())])
}

fn level_color(pct: f64) -> Color {
    if pct >= 90.0 { Color::Red } else if pct >= 70.0 { Color::Yellow } else { Color::Green }
}

fn temp_color(t: f64) -> Color {
    if t >= 85.0 { Color::Red } else if t >= 70.0 { Color::Yellow } else { Color::Green }
}

fn opt(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.0}{unit}")).unwrap_or_else(|| "—".into())
}

fn waiting() -> Vec<Line<'static>> {
    vec![Line::from(Span::styled("Coletando dados...", Style::new().fg(MUTED)))]
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.width < 60 || area.height < 14 {
        let msg = format!("Terminal pequeno ({}x{}).\nAumente a janela para pelo menos 60x14.\n(q sai)", area.width, area.height);
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }), area);
        return;
    }
    let [top, body, foot] = Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(2)]).areas(area);
    let titles: Vec<Line> = TABS.iter().enumerate().map(|(i, t)| Line::from(format!("{} {}", i + 1, t.title()))).collect();
    let tabs = Tabs::new(titles).select(app.tab.index()).block(Block::bordered().title(" RESET EXPLODE ")).highlight_style(Style::new().fg(Color::Black).bg(ACCENT)).divider("│");
    f.render_widget(tabs, top);
    match app.tab {
        Tab::Resumo => resumo(f, app, body),
        Tab::Sensores => scrolled(f, app, body, "Sensores", sensor_lines(app)),
        Tab::Discos => scrolled(f, app, body, "Discos e volumes", disk_lines(app)),
        Tab::Gpu => scrolled(f, app, body, "Placas de vídeo", gpu_lines(app)),
        Tab::Rede => scrolled(f, app, body, "Energia, rede e portas seriais", net_lines(app)),
        Tab::Testes => tests(f, app, body),
        Tab::Jogos => games(f, app, body),
        Tab::Ferramentas => tools(f, app, body),
    }
    footer(f, app, foot);
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    if let Some((lv, t)) = &app.status {
        let c = match lv { Level::Info => ACCENT, Level::Ok => Color::Green, Level::Err => Color::Red };
        f.render_widget(Paragraph::new(Span::styled(trunc(t, area.width as usize), Style::new().fg(c))), l1);
    }
    let hint = match app.tab {
        Tab::Testes => "↑↓ escolher · Enter iniciar · Esc cancela · Tab/1-8 abas · q sai",
        Tab::Jogos => "↑↓ · g gamemode · m mangohud · d dGPU · p perf. · i prioridade · Enter iniciar · r recarregar · q sai",
        Tab::Ferramentas => "←→ painel · ↑↓ · Enter aplica/desfaz/instala · r atualizar · Tab/1-8 abas · q sai",
        _ => "↑↓ PgUp/PgDn rolar · ←→ ou Tab/1-8 trocar de aba · q sai",
    };
    f.render_widget(Paragraph::new(Span::styled(trunc(hint, area.width as usize), Style::new().fg(MUTED))), l2);
}

/// Texto com rolagem; calcula a rolagem máxima para o teclado não passar do fim.
fn scrolled(f: &mut Frame, app: &App, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let view = area.height.saturating_sub(2);
    let max = (lines.len() as u16).saturating_sub(view);
    app.max_scroll.set(max);
    let y = app.scroll[app.tab.index()].min(max);
    let shown = if max > 0 { format!(" {title} ({}/{}) ", y + 1, max + 1) } else { format!(" {title} ") };
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(shown)).scroll((y, 0)), area);
}

// ---------------------------------------------------------------------------------------------
// Resumo
// ---------------------------------------------------------------------------------------------

pub fn system_lines(app: &App) -> Vec<Line<'static>> {
    let Some(s) = &app.stat else { return waiting() };
    let mut v = vec![kv("Sistema", format!("{} · {}", s.os.pretty_name, s.os.arch)), kv("Kernel", s.os.kernel.clone()), kv("Máquina", s.os.hostname.clone())];
    v.push(kv("Ligado há", fmt_duration(s.os.uptime_secs)));
    if let Some(vm) = &s.os.virtualization {
        v.push(kv("Virtualização", vm.clone()));
    }
    let board = format!("{} {}", s.board.board_vendor, s.board.board_name);
    if !board.trim().is_empty() {
        v.push(kv("Placa-mãe", board.trim().to_string()));
    }
    v.push(kv("CPU", s.cpu.model.clone()));
    v.push(kv("Núcleos", format!("{} / {} threads", s.cpu.cores, s.cpu.threads)));
    if let Some(l) = &app.live {
        v.push(kv("Memória", format!("{} total · {} livre", fmt_bytes(l.mem.total), fmt_bytes(l.mem.available))));
        for g in &l.gpus {
            v.push(kv("GPU", g.name.clone()));
        }
    }
    for d in &s.displays {
        if let Some(e) = &d.edid {
            let size = e.diagonal_inches().map(|i| format!(" {i:.0}\""));
            let res = e.preferred.map(|(w, h, hz)| format!(" {w}x{h}@{hz:.0}Hz"));
            v.push(kv("Tela", format!("{}{}{} ({})", e.name.clone().unwrap_or_else(|| e.manufacturer.clone()), size.unwrap_or_default(), res.unwrap_or_default(), d.connector)));
        }
    }
    v
}

fn resumo(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    f.render_widget(Paragraph::new(system_lines(app)).block(Block::bordered().title(" Sistema ")).wrap(Wrap { trim: true }), left);
    let block = Block::bordered().title(" Agora ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let Some(l) = &app.live else {
        f.render_widget(Paragraph::new(waiting()), inner);
        return;
    };
    let [g1, g2, g3, spark, rest] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(4), Constraint::Min(0)]).areas(inner);
    gauge(f, g1, "CPU ", l.cpu.total, format!("{:.0}%", l.cpu.total));
    let used = l.mem.total.saturating_sub(l.mem.available);
    let mem_pct = if l.mem.total > 0 { used as f64 / l.mem.total as f64 * 100.0 } else { 0.0 };
    gauge(f, g2, "RAM ", mem_pct, format!("{} / {}", fmt_bytes(used), fmt_bytes(l.mem.total)));
    let swap_used = l.mem.swap_total.saturating_sub(l.mem.swap_free);
    let swap_pct = if l.mem.swap_total > 0 { swap_used as f64 / l.mem.swap_total as f64 * 100.0 } else { 0.0 };
    gauge(f, g3, "Swap", swap_pct, if l.mem.swap_total == 0 { "sem swap".into() } else { format!("{} / {}", fmt_bytes(swap_used), fmt_bytes(l.mem.swap_total)) });
    f.render_widget(Sparkline::default().data(&app.cpu_history).max(100).style(Style::new().fg(ACCENT)).block(Block::new().borders(Borders::TOP).title(" CPU (últimos minutos) ")), spark);
    let mut lines = vec![];
    if let Some(t) = crate::sensors::cpu_temperature(&l.sensors) {
        lines.push(Line::from(vec![Span::styled(format!("{:<14}", "Temp. CPU"), Style::new().fg(MUTED)), Span::styled(format!("{t:.0} °C"), Style::new().fg(temp_color(t)))]));
    }
    for g in &l.gpus {
        lines.push(kv("GPU", format!("{} · uso {} · temp {} · {}", trunc(&g.name, 20), opt(g.util_gpu, "%"), opt(g.temp, " °C"), opt(g.power_w, " W"))));
    }
    if let Some(b) = l.power.batteries.first() {
        lines.push(kv("Bateria", format!("{} · {}", opt(b.capacity, "%"), b.status)));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("Sem sensores disponíveis (comum em máquina virtual).", Style::new().fg(MUTED))));
    }
    f.render_widget(Paragraph::new(lines), rest);
}

fn gauge(f: &mut Frame, area: Rect, name: &str, pct: f64, label: String) {
    let [n, g] = Layout::horizontal([Constraint::Length(5), Constraint::Min(5)]).areas(area);
    f.render_widget(Paragraph::new(name.to_string()), n);
    f.render_widget(Gauge::default().ratio((pct / 100.0).clamp(0.0, 1.0)).label(label).gauge_style(Style::new().fg(level_color(pct)).bg(Color::Black)), g);
}

// ---------------------------------------------------------------------------------------------
// Textos
// ---------------------------------------------------------------------------------------------

pub fn sensor_lines(app: &App) -> Vec<Line<'static>> {
    let Some(l) = &app.live else { return waiting() };
    if l.sensors.is_empty() {
        return vec![Line::from("Nenhum sensor encontrado (máquina virtual ou sem drivers hwmon)."), Line::from(Span::styled("Dica: instale e configure o lm-sensors (aba Ferramentas).", Style::new().fg(MUTED)))];
    }
    let mut v = vec![];
    let mut last = String::new();
    for s in &l.sensors {
        if s.chip_display != last {
            if !last.is_empty() {
                v.push(Line::from(""));
            }
            v.push(heading(&s.chip_display));
            last = s.chip_display.clone();
        }
        let (val, color) = match s.kind {
            SensorKind::Temp => (format!("{:.1} °C", s.value), temp_color(s.value)),
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
    let mut v = vec![];
    match &app.stat {
        None => return waiting(),
        Some(s) => {
            v.push(heading("Discos"));
            for d in &s.disks {
                v.push(Line::from(format!("{} · {} · {} · {} · {}", d.name, d.title(), fmt_bytes(d.size), d.kind(), d.bus)));
                v.push(Line::from(Span::styled(format!("    temp {} · {}", opt(d.temp, " °C"), d.verdict.label()), Style::new().fg(MUTED))));
            }
            if s.disks.is_empty() {
                v.push(Line::from("Nenhum disco encontrado."));
            }
        }
    }
    if let Some(l) = &app.live {
        v.push(Line::from(""));
        v.push(heading("Volumes montados"));
        for m in &l.mounts {
            let pct = if m.total > 0 { m.used as f64 / m.total as f64 * 100.0 } else { 0.0 };
            v.push(Line::from(vec![Span::raw(format!("{:<26} {:<7} ", trunc(&m.mountpoint, 26), m.fstype)), Span::styled(format!("{:>3.0}%", pct), Style::new().fg(level_color(pct))), Span::raw(format!("  {} de {}", fmt_bytes(m.used), fmt_bytes(m.total)))]));
        }
    }
    v.push(Line::from(""));
    v.push(Line::from(Span::styled("SMART detalhado: `resetexplode disks --smart` (precisa de sudo).", Style::new().fg(MUTED))));
    v
}

pub fn gpu_lines(app: &App) -> Vec<Line<'static>> {
    let Some(l) = &app.live else { return waiting() };
    if l.gpus.is_empty() {
        return vec![Line::from("Nenhuma GPU encontrada.")];
    }
    let mut v = vec![];
    for g in &l.gpus {
        v.push(heading(&format!("[{}] {} ({})", g.index, g.name, g.vendor.name())));
        v.push(kv("Driver", g.driver_version.clone().or(g.driver.clone()).unwrap_or_else(|| "—".into())));
        v.push(kv("VRAM", format!("{} / {}", g.vram_used.map(fmt_bytes).unwrap_or_else(|| "—".into()), g.vram_total.map(fmt_bytes).unwrap_or_else(|| "—".into()))));
        v.push(kv("Uso", opt(g.util_gpu, "%")));
        v.push(kv("Temperatura", opt(g.temp, " °C")));
        v.push(kv("Consumo", format!("{} (limite {})", opt(g.power_w, " W"), opt(g.power_limit_w, " W"))));
        v.push(kv("Clock", format!("núcleo {} · memória {}", opt(g.clock_core, " MHz"), opt(g.clock_mem, " MHz"))));
        v.push(Line::from(""));
    }
    v
}

pub fn net_lines(app: &App) -> Vec<Line<'static>> {
    let mut v = vec![heading("Energia")];
    match &app.live {
        None => return waiting(),
        Some(l) => {
            v.push(kv("Tomada", match l.power.on_ac { Some(true) => "ligada", Some(false) => "desligada (na bateria)", None => "—" }));
            if l.power.batteries.is_empty() {
                v.push(Line::from("Sem bateria (desktop)."));
            }
            for b in &l.power.batteries {
                v.push(kv(&b.name, format!("{} · {} · saúde {} · {} ciclos", opt(b.capacity, "%"), b.status, opt(b.health, "%"), b.cycles.map(|c| c.to_string()).unwrap_or_else(|| "—".into()))));
            }
            let c = &l.power.cpu;
            v.push(kv("Governador", match &c.governor {
                Some(g) => format!("{g} ({})", c.governors.join("/")),
                None => "não exposto por este sistema".into(),
            }));
            v.push(kv("Boost", c.boost.map(|b| if b { "ligado" } else { "desligado" }).unwrap_or("—")));
            v.push(kv("Perfil", c.platform_profile.clone().unwrap_or_else(|| "—".into())));
        }
    }
    if let Some(s) = &app.stat {
        v.push(Line::from(""));
        v.push(heading("Rede"));
        for i in &s.interfaces {
            v.push(Line::from(format!("{:<12} {:<9} {:<8} {} {}", i.name, i.kind, if i.up { "ativa" } else { "inativa" }, i.speed_mbps.map(|s| format!("{s} Mb/s")).unwrap_or_default(), i.driver.clone().unwrap_or_default())));
        }
        v.push(kv("DNS", if s.dns.is_empty() { "—".to_string() } else { s.dns.join(", ") }));
        v.push(Line::from(Span::styled("Latência, velocidade e bufferbloat: `resetexplode net --test`.", Style::new().fg(MUTED))));
        v.push(Line::from(""));
        v.push(heading("Portas seriais USB"));
        for p in &s.ports {
            v.push(Line::from(format!("{} · {} {} · {}{}", p.device, p.vendor_id.clone().unwrap_or_default(), p.product_id.clone().unwrap_or_default(), p.product.clone().unwrap_or_default(), if p.accessible { "" } else { " · SEM ACESSO" })));
        }
        for a in &s.serial_advice {
            v.push(Line::from(Span::styled(format!("! {}", a.problem), Style::new().fg(Color::Yellow))));
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
    let [left, right] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let items: Vec<ListItem> = jobs::ALL.iter().map(|k| ListItem::new(k.title())).collect();
    let list = List::new(items).block(Block::bordered().title(" Testes ")).highlight_style(Style::new().fg(Color::Black).bg(ACCENT)).highlight_symbol("▶ ");
    f.render_stateful_widget(list, left, &mut list_state(app.job_sel));

    let block = Block::bordered().title(" Detalhes ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let kind = jobs::ALL[app.job_sel.min(jobs::ALL.len() - 1)];
    let [desc, prog, res] = Layout::vertical([Constraint::Length(4), Constraint::Length(3), Constraint::Min(0)]).areas(inner);
    let mut d = vec![Line::from(kind.desc())];
    if kind.heavy() {
        d.push(Line::from(Span::styled("Carga pesada: o PC pode esquentar e as ventoinhas vão subir.", Style::new().fg(Color::Yellow))));
    }
    f.render_widget(Paragraph::new(d).wrap(Wrap { trim: true }), desc);
    if let Some(j) = &app.job {
        running(f, j, prog);
    } else {
        f.render_widget(Paragraph::new(Span::styled("Enter inicia o teste selecionado.", Style::new().fg(MUTED))), prog);
    }
    let mut lines = vec![];
    if let Some((k, done)) = &app.job_result {
        lines.push(heading(&format!("Resultado: {}", k.title())));
        lines.extend(done.lines.iter().map(|l| Line::from(l.clone())));
        if done.cancelled {
            lines.push(Line::from(Span::styled("Cancelado: resultado parcial, não foi salvo.", Style::new().fg(Color::Yellow))));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), res);
}

fn running(f: &mut Frame, j: &Running, area: Rect) {
    let [a, b] = Layout::vertical([Constraint::Length(1), Constraint::Length(2)]).areas(area);
    f.render_widget(Paragraph::new(Span::styled(format!("Executando: {} · {}", j.kind.title(), fmt_duration(j.started.elapsed().as_secs())), Style::new().fg(Color::Green))), a);
    f.render_widget(Gauge::default().ratio(j.fraction().clamp(0.0, 1.0)).label(trunc(&j.label(), area.width as usize)).gauge_style(Style::new().fg(Color::Green).bg(Color::Black)), b);
}

// ---------------------------------------------------------------------------------------------
// Jogos
// ---------------------------------------------------------------------------------------------

fn flag(on: bool) -> &'static str {
    if on { "[x]" } else { "[ ]" }
}

fn games(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
    if app.games.is_empty() {
        f.render_widget(Paragraph::new(vec![Line::from("Nenhum jogo encontrado (Steam, Heroic, Lutris)."), Line::from(""), Line::from("Adicione um manualmente com:"), Line::from("  resetexplode games add \"Nome\" /caminho/do/executavel")]).block(Block::bordered().title(" Jogos ")).wrap(Wrap { trim: true }), area);
        return;
    }
    let items: Vec<ListItem> = app.games.iter().map(|g| ListItem::new(format!("{} · {}", trunc(&g.name, 30), g.source.name()))).collect();
    let list = List::new(items).block(Block::bordered().title(format!(" Jogos ({}) ", app.games.len()))).highlight_style(Style::new().fg(Color::Black).bg(ACCENT)).highlight_symbol("▶ ");
    f.render_stateful_widget(list, left, &mut list_state(app.game_sel));
    let Some(g) = app.games.get(app.game_sel) else { return };
    let p = app.profile_of(g);
    let mut v = vec![heading(&g.name), kv("Origem", g.source.name()), kv("Pasta", g.install_dir.clone().unwrap_or_else(|| "—".into())), Line::from("")];
    v.push(Line::from(format!("{} GameMode (g)   {} MangoHud (m)   {} GPU dedicada (d)", flag(p.gamemode), flag(p.mangohud), flag(p.dgpu))));
    v.push(Line::from(format!("{} Governador performance (p)   Prioridade (i): {}", flag(p.performance_governor), p.priority)));
    v.push(Line::from(""));
    match launch::plan(&p, app.live.as_ref().and_then(|l| crate::gpu::primary(&l.gpus)).map(|g| g.vendor), crate::util::is_root(), &crate::util::have) {
        Ok(pl) if g.steam_appid.is_some() => {
            v.push(heading("Opções de inicialização da Steam"));
            v.push(Line::from(launch::steam_options(&pl)));
            v.extend(pl.warnings.iter().map(|w| Line::from(Span::styled(format!("aviso: {w}"), Style::new().fg(Color::Yellow)))));
        }
        Ok(pl) => {
            v.push(Line::from(Span::styled("Enter inicia o jogo com este perfil.", Style::new().fg(Color::Green))));
            v.extend(pl.warnings.iter().map(|w| Line::from(Span::styled(format!("aviso: {w}"), Style::new().fg(Color::Yellow)))));
        }
        Err(e) => v.push(Line::from(Span::styled(format!("perfil inválido: {e}"), Style::new().fg(Color::Red)))),
    }
    f.render_widget(Paragraph::new(v).block(Block::bordered().title(" Perfil ")).wrap(Wrap { trim: false }), right);
}

// ---------------------------------------------------------------------------------------------
// Ferramentas
// ---------------------------------------------------------------------------------------------

fn tools(f: &mut Frame, app: &App, area: Rect) {
    let panels = Layout::vertical([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)]).split(area);
    let titles = [" Ajustes de sistema (valem até reiniciar) ", " Programas de inicialização ", " Instalar utilitários "];
    let rows: [Vec<ListItem>; 3] = [
        app.tweaks.iter().map(|t| ListItem::new(format!("{} {:<20} {} → {}  {}", if t.applied { "[✓]" } else { "[ ]" }, t.id, t.current.clone().unwrap_or_else(|| "—".into()), t.recommended, if !t.available { "(indisponível)" } else if t.can_restore { "(restaurável)" } else { "" }))).collect(),
        app.startup.iter().map(|e| ListItem::new(format!("{} {:<30} {}", if e.enabled { "[✓]" } else { "[ ]" }, trunc(&e.name, 30), if e.system { "(sistema)" } else { "" }))).collect(),
        app.apps.iter().map(|a| ListItem::new(format!("{} {:<14} {}", if a.installed { "[✓]" } else { "[ ]" }, a.id, a.title))).collect(),
    ];
    for (i, items) in rows.into_iter().enumerate() {
        let focused = app.tool_panel == i;
        let border = if focused { Style::new().fg(ACCENT) } else { Style::new().fg(MUTED) };
        let empty = items.is_empty();
        let list = List::new(items).block(Block::bordered().border_style(border).title(titles[i])).highlight_style(if focused { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new() }).highlight_symbol(if focused { "▶ " } else { "  " });
        if empty {
            f.render_widget(Paragraph::new("(vazio)").block(Block::bordered().border_style(border).title(titles[i])), panels[i]);
        } else {
            f.render_stateful_widget(list, panels[i], &mut list_state(app.tool_sel[i]));
        }
    }
}
