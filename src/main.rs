use clap::Parser;

fn main() {
    // Sem subcomando, `cli::run` abre a interface interativa (TUI).
    let cli = resetexplode::cli::Cli::parse();
    if let Err(e) = resetexplode::cli::run(cli) {
        eprintln!("erro: {e:#}");
        std::process::exit(1);
    }
}
