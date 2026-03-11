use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about = "Простой CLI для прогноза погоды", long_about = None)]
pub struct Args {
    /// Название города
    #[arg(short, long)]
    pub city: String,
}
