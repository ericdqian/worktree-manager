use clap::Parser;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("worktree-manager");
}
