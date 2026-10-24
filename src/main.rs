use clap::Parser;

fn main() {
    let cli = resetexplode::cli::Cli::parse();
    if let Err(e) = resetexplode::cli::run(cli) {
        eprintln!("erro: {e:#}");
        std::process::exit(1);
    }
}
