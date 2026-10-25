use clap::Parser;

mod tui;

fn main() {
    let cli = resetexplode::cli::Cli::parse();

    // Se nenhum subcomando foi passado, abre a TUI interativa
    if cli.command.is_none() {
        if let Err(e) = tui::run() {
            eprintln!("falha ao iniciar a interface: {}", e);
        }
        std::process::exit(0);
    }

    if let Err(e) = resetexplode::cli::run(cli) {
        eprintln!("erro: {e:#}");
        std::process::exit(1);
    }
}
