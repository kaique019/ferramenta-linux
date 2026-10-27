use clap::Parser;

fn main() {
    // Sem subcomando, `cli::run` abre a interface interativa (TUI).
    let cli = pingu::cli::Cli::parse();
    if let Err(e) = pingu::cli::run(cli) {
        eprintln!("erro: {e:#}");
        std::process::exit(1);
    }
}
