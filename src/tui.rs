//! Interface TUI (menu) para o binário `resetexplode`
//! Usa as crates já declaradas no Cargo.toml: ratatui + crossterm (re-exportado por ratatui)

use std::process::Command;
use std::time::Duration;

use ratatui::crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem},
    Terminal,
};

/// Sub‑comandos que o binário aceita (mesmo nome que aparece na ajuda).
const MENU_ITEMS: &[&str] = &[
    "Info", "Sensors", "Gpu", "Disks", "Pci", "Power", "Net", "Errors",
    "Games", "Report", "Startup", "Tweaks", "Serial", "Install",
    "Overlay", "Oc", "Bench", "Stress",
];

/// Executa o sub‑comando escolhido.
/// Primeiro sai do modo raw / tela alternada, depois chama o próprio binário
/// com o argumento em minúsculas (ex.: `resetexplode info`), e por fim volta ao modo raw.
fn run_subcommand(name: &str) {
    // 1️⃣  Restaurar terminal
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);

    // 2️⃣  Chamar sub‑comando
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("não foi possível obter o caminho do executável: {e}");
        std::process::exit(1);
    });

    let status = Command::new(exe)
        .arg(name.to_ascii_lowercase())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("falha ao executar sub‑comando `{name}`: {e}");
            std::process::exit(1);
        });

    if !status.success() {
        eprintln!("sub‑comando `{name}` terminou com código {}", status);
    }

    // 3️⃣  Voltar ao modo TUI
    enable_raw_mode().expect("não foi possível reativar raw mode");
    execute!(std::io::stdout(), EnterAlternateScreen)
        .expect("não foi possível entrar no alternate screen");
}

/// Loop principal da TUI. Retorna `Ok(())` quando o usuário sai (Esc/q) ou ocorre erro interno.
pub fn run() -> std::io::Result<()> {
    // ---------- prepara terminal ----------
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ---------- estado da UI ----------
    let mut selected: usize = 0;

    loop {
        // ---------- render ----------
        terminal.draw(|frame| {
            let area = frame.area();
            let block = Block::default()
                .title(
                    "ResetExplode – Menu (↑/↓ = navegar, Enter = executar, q/Esc = sair)",
                )
                .borders(Borders::ALL);

            let items: Vec<ListItem> = MENU_ITEMS
                .iter()
                .enumerate()
                .map(|(i, &txt)| {
                    let style = if i == selected {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    ListItem::new(txt).style(style)
                })
                .collect();

            let list = List::new(items).block(block);
            frame.render_widget(list, area);
        })?;

        // ---------- eventos ----------
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up => {
                        if selected > 0 {
                            selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected + 1 < MENU_ITEMS.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let cmd_name = MENU_ITEMS[selected];
                        run_subcommand(cmd_name);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    // ---------- limpeza ----------
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}