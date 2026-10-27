//! Desenho das telas. Só lê o estado (`App`); nunca coleta dados nem executa nada.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Sparkline, Tabs, Wrap};

use super::jobs::{self, Running};
use super::theme::Theme;
use super::{App, Level, TABS, Tab};
use crate::launch;
use crate::sensors::SensorKind;
use crate::util::{fmt_bytes, fmt_duration, fmt_mhz, trunc};

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

pub fn draw(f: &mut Frame, app: &App) {
    let th = &app.theme;
    let area = f.area();
    if area.width < 60 || area.height < 14 {
        let msg = format!("Terminal pequeno ({}x{}).\nAumente a janela para pelo menos 60x14.\n(q sai)", area.width, area.height);
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }), area);
        return;
    }
    let [top, body, foot] = Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(2)]).areas(area);
    let titles: Vec<Line> = TABS.iter().enumerate().map(|(i, t)| Line::from(format!("{} {}", i + 1, t.title()))).collect();
    let tabs = Tabs::new(titles).select(app.tab.index()).block(th.block(" PINGU ")).highlight_style(th.selection()).divider(th.divider.as_str());
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
    let th = &app.theme;
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    if let Some((lv, t)) = &app.status {
        let c = match lv { Level::Info => th.accent, Level::Ok => th.ok, Level::Err => th.err };
        f.render_widget(Paragraph::new(Span::styled(trunc(t, area.width as usize), Style::new().fg(c))), l1);
    }
    let hint = match app.tab {
        Tab::Testes => "↑↓ escolher · Enter iniciar · Esc cancela · Tab/1-8 abas · q sai",
        Tab::Jogos => "↑↓ · g gamemode · m mangohud · d dGPU · p perf. · i prioridade · Enter iniciar · r recarregar · q sai",
        Tab::Ferramentas => "←→ painel · ↑↓ · Enter aplica/desfaz/instala · r atualizar · Tab/1-8 abas · q sai",
        _ => "↑↓ PgUp/PgDn rolar · ←→ ou Tab/1-8 trocar de aba · t tema · q sai",
    };
    f.render_widget(Paragraph::new(Span::styled(trunc(hint, area.width as usize), th.muted())), l2);
}

/// Texto com rolagem; calcula a rolagem máxima para o teclado não passar do fim.
fn scrolled(f: &mut Frame, app: &App, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let th = &app.theme;
    let view = area.height.saturating_sub(2);
    let max = (lines.len() as u16).saturating_sub(view);
    app.max_scroll.set(max);
    let y = app.scroll[app.tab.index()].min(max);
    let shown = if max > 0 { format!(" {title} ({}/{}) ", y + 1, max + 1) } else { format!(" {title} ") };
    f.render_widget(Paragraph::new(lines).block(th.block(shown)).scroll((y, 0)), area);
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
    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    f.render_widget(Paragraph::new(system_lines(app)).block(th.block(" Sistema ")).wrap(Wrap { trim: true }), left);
    let block = th.block(" Agora ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let Some(l) = &app.live else {
        f.render_widget(Paragraph::new(waiting(th)), inner);
        return;
    };
    let [g1, g2, g3, spark, rest] = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Length(4), Constraint::Min(0)]).areas(inner);
    gauge(f, th, g1, "CPU ", l.cpu.total, format!("{:.0}%", l.cpu.total));
    let used = l.mem.total.saturating_sub(l.mem.available);
    let mem_pct = if l.mem.total > 0 { used as f64 / l.mem.total as f64 * 100.0 } else { 0.0 };
    gauge(f, th, g2, "RAM ", mem_pct, format!("{} / {}", fmt_bytes(used), fmt_bytes(l.mem.total)));
    let swap_used = l.mem.swap_total.saturating_sub(l.mem.swap_free);
    let swap_pct = if l.mem.swap_total > 0 { swap_used as f64 / l.mem.swap_total as f64 * 100.0 } else { 0.0 };
    gauge(f, th, g3, "Swap", swap_pct, if l.mem.swap_total == 0 { "sem swap".into() } else { format!("{} / {}", fmt_bytes(swap_used), fmt_bytes(l.mem.swap_total)) });
    f.render_widget(Sparkline::default().data(&app.cpu_history).max(100).style(th.accent_style()).block(Block::new().borders(Borders::TOP).title(" CPU (últimos minutos) ")), spark);
    let mut lines = vec![];
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
            v.push(Line::from(vec![Span::raw(format!("{:<26} {:<7} ", trunc(&m.mountpoint, 26), m.fstype)), Span::styled(format!("{:>3.0}%", pct), Style::new().fg(th.level_color(pct))), Span::raw(format!("  {} de {}", fmt_bytes(m.used), fmt_bytes(m.total)))]));
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
    let [left, right] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
    let items: Vec<ListItem> = jobs::ALL.iter().map(|k| ListItem::new(k.title())).collect();
    let list = List::new(items).block(th.block(" Testes ")).highlight_style(th.selection()).highlight_symbol(th.pointer.as_str());
    f.render_stateful_widget(list, left, &mut list_state(app.job_sel));

    let block = th.block(" Detalhes ");
    let inner = block.inner(right);
    f.render_widget(block, right);
    let kind = jobs::ALL[app.job_sel.min(jobs::ALL.len() - 1)];
    let [desc, prog, res] = Layout::vertical([Constraint::Length(4), Constraint::Length(3), Constraint::Min(0)]).areas(inner);
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
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
    if app.games.is_empty() {
        f.render_widget(Paragraph::new(vec![Line::from("Nenhum jogo encontrado (Steam, Heroic, Lutris)."), Line::from(""), Line::from("Adicione um manualmente com:"), Line::from("  pingu games add \"Nome\" /caminho/do/executavel")]).block(th.block(" Jogos ")).wrap(Wrap { trim: true }), area);
        return;
    }
    let items: Vec<ListItem> = app.games.iter().map(|g| ListItem::new(format!("{} · {}", trunc(&g.name, 30), g.source.name()))).collect();
    let list = List::new(items).block(th.block(format!(" Jogos ({}) ", app.games.len()))).highlight_style(th.selection()).highlight_symbol(th.pointer.as_str());
    f.render_stateful_widget(list, left, &mut list_state(app.game_sel));
    let Some(g) = app.games.get(app.game_sel) else { return };
    let p = app.profile_of(g);
    let mut v = vec![heading(th, &g.name), kv(th, "Origem", g.source.name()), kv(th, "Pasta", g.install_dir.clone().unwrap_or_else(|| "—".into())), Line::from("")];
    v.push(Line::from(format!("{} GameMode (g)   {} MangoHud (m)   {} GPU dedicada (d)", th.check(p.gamemode), th.check(p.mangohud), th.check(p.dgpu))));
    v.push(Line::from(format!("{} Governador performance (p)   Prioridade (i): {}", th.check(p.performance_governor), p.priority)));
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
    f.render_widget(Paragraph::new(v).block(th.block(" Perfil ")).wrap(Wrap { trim: false }), right);
}

// ---------------------------------------------------------------------------------------------
// Ferramentas
// ---------------------------------------------------------------------------------------------

fn tools(f: &mut Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let panels = Layout::vertical([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)]).split(area);
    let titles = [" Ajustes de sistema (valem até reiniciar) ", " Programas de inicialização ", " Instalar utilitários "];
    let rows: [Vec<ListItem>; 3] = [
        app.tweaks.iter().map(|t| ListItem::new(format!("{} {:<20} {} → {}  {}", th.check(t.applied), t.id, t.current.clone().unwrap_or_else(|| "—".into()), t.recommended, if !t.available { "(indisponível)" } else if t.can_restore { "(restaurável)" } else { "" }))).collect(),
        app.startup.iter().map(|e| ListItem::new(format!("{} {:<30} {}", th.check(e.enabled), trunc(&e.name, 30), if e.system { "(sistema)" } else { "" }))).collect(),
        app.apps.iter().map(|a| ListItem::new(format!("{} {:<14} {}", th.check(a.installed), a.id, a.title))).collect(),
    ];
    for (i, items) in rows.into_iter().enumerate() {
        let focused = app.tool_panel == i;
        let empty = items.is_empty();
        let blank = " ".repeat(th.pointer.chars().count());
        let list = List::new(items).block(th.focus_block(titles[i], focused)).highlight_style(if focused { th.selection() } else { Style::new() }).highlight_symbol(if focused { th.pointer.as_str() } else { blank.as_str() });
        if empty {
            f.render_widget(Paragraph::new("(vazio)").block(th.focus_block(titles[i], focused)), panels[i]);
        } else {
            f.render_stateful_widget(list, panels[i], &mut list_state(app.tool_sel[i]));
        }
    }
}
