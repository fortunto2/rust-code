use autoresearch::{AutoResearch, Config, Target};
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let target_name = args.get(1).map(|s| s.as_str()).unwrap_or("tool-selection");
    let cycles: usize = args
        .iter()
        .position(|a| a == "--cycles")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let batch: usize = args
        .iter()
        .position(|a| a == "--batch")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let target = match target_name {
        "tool-selection" => Target::ToolSelection,
        "system-prompt" => Target::SystemPrompt,
        "decision-parser" => Target::DecisionParser,
        name if name.starts_with("skill-") => Target::Skill {
            name: name.strip_prefix("skill-").unwrap().into(),
            skill_path: None,
        },
        other => {
            eprintln!("Unknown target: {other}");
            eprintln!("Usage: autoresearch <tool-selection|system-prompt|decision-parser|skill-NAME> [--cycles N] [--batch N]");
            std::process::exit(1);
        }
    };

    let data_dir = PathBuf::from("skills/autoresearch/data").join(target.name());

    let config = Config {
        target,
        batch_size: batch,
        data_dir,
        ..Default::default()
    };

    let ar = AutoResearch::new(config);
    if let Err(e) = ar.run(cycles).await {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}
