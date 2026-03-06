pub mod tui;
pub mod app;
pub mod preview;
pub mod agent;
pub mod tools;
pub mod baml_client;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crate::agent::{Agent, AgentEvent};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    prompt: Option<String>,

    #[arg(short, long, default_value_t = false)]
    resume: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Manage installed skills
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Show MCP server status and tools
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand, Debug)]
enum McpAction {
    /// List configured MCP servers and their tools
    List,
    /// Show .mcp.json config
    Config,
}

#[derive(Subcommand, Debug)]
enum SkillsAction {
    /// List installed skills
    List,
    /// Show full content of a skill (SKILL.md + extras)
    Show {
        /// Skill name
        name: String,
        /// Show only SKILL.md without supplementary files
        #[arg(short, long)]
        brief: bool,
    },
    /// Install a skill from a repository (e.g. owner/repo)
    Add {
        /// Repository in owner/repo format
        repo: String,
    },
    /// Remove an installed skill by name
    Remove {
        /// Skill name to remove
        name: String,
    },
    /// Search remote skills on skills.sh
    Search {
        /// Search query
        query: String,
    },
    /// Find which installed skills match a message/query
    Match {
        /// Message to match against skill descriptions
        message: String,
    },
    /// Browse full skills.sh catalog (cached locally)
    Catalog {
        /// Force refresh from skills.sh (ignore cache)
        #[arg(short, long)]
        refresh: bool,
        /// Search query to filter catalog
        query: Option<String>,
    },
}

fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = tracing_appender::rolling::never(".", "rust-code.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();

    // Also suppress BAML's default stdout logging
    unsafe {
        std::env::set_var("BAML_LOG", "off");
    }

    guard
}

fn setup_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal so panic message is readable
        let _ = tui::restore();
        original_hook(panic_info);
    }));
}

async fn run_skills_command(action: SkillsAction) -> Result<()> {
    match action {
        SkillsAction::List => {
            // Ensure default skills
            let missing = tools::check_default_skills();
            if !missing.is_empty() {
                for (name, repo) in &missing {
                    println!("Installing default skill '{}' from {}...", name, repo);
                }
                let installed = tools::ensure_default_skills().await;
                if !installed.is_empty() {
                    println!("Installed defaults: {}\n", installed.join(", "));
                }
            }

            let skills = tools::collect_installed_skills();
            if skills.is_empty() {
                println!("No skills installed.");
                println!("\nSkill directories searched:");
                println!("  ~/.agents/skills  (canonical)");
                println!("  ~/.claude/skills");
                println!("  .agents/skills    (project-local)");
                println!("  .claude/skills    (project-local)");
                println!("\nInstall skills: rust-code skills add <owner/repo>");
                return Ok(());
            }
            println!("Installed skills ({}):\n", skills.len());
            for skill in &skills {
                let scope = match skill.source {
                    tools::SkillSource::Global => "global",
                    tools::SkillSource::Project => "project",
                };
                print!("  {} [{}]", skill.name, scope);
                if let Some(desc) = &skill.description {
                    print!(" - {}", desc);
                }
                println!();
                println!("    {}", skill.path.display());
            }
        }
        SkillsAction::Add { repo } => {
            println!("Installing skill from {}...", repo);
            match tools::install_skill(&repo).await {
                Ok(output) => {
                    if output.trim().is_empty() {
                        println!("Installed successfully.");
                    } else {
                        println!("{}", output);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to install: {}", e);
                    std::process::exit(1);
                }
            }
        }
        SkillsAction::Remove { name } => {
            match tools::remove_skill(&name) {
                Ok(()) => println!("Removed skill '{}'.", name),
                Err(e) => {
                    eprintln!("Failed to remove '{}': {}", name, e);
                    std::process::exit(1);
                }
            }
        }
        SkillsAction::Show { name, brief } => {
            match if brief {
                tools::read_skill_content(&name)
            } else {
                tools::load_skill_full(&name)
            } {
                Ok(content) => println!("{}", content),
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        SkillsAction::Search { query } => {
            // Fuzzy search local skills first
            let local_results = tools::fuzzy_search_skills(&query);
            let local_hits: Vec<_> = local_results.iter().filter(|(s, _)| *s >= 50).collect();
            if !local_hits.is_empty() {
                println!("Installed matches:\n");
                for (score, skill) in &local_hits {
                    let scope = match skill.source {
                        tools::SkillSource::Global => "global",
                        tools::SkillSource::Project => "project",
                    };
                    print!("  {} [{}] (score: {})", skill.name, scope, score);
                    if let Some(desc) = &skill.description {
                        print!(" - {}", desc);
                    }
                    println!();
                }
                println!();
            }

            // Search skills.sh API (60K+ skills)
            let remote = tools::search_skills_api(&query);
            if remote.is_empty() && local_hits.is_empty() {
                println!("No results found.");
            } else if !remote.is_empty() {
                println!("Remote results ({}):\n", remote.len());
                for entry in &remote {
                    println!("  {} ({})  {} installs", entry.name, entry.repo, entry.installs);
                }
                println!("\nInstall: rust-code skills add <owner/repo/skill-name>");
            }
        }
        SkillsAction::Match { message } => {
            let matched = tools::match_skills_for_message(&message);
            if matched.is_empty() {
                println!("No skills matched for: {}", message);
            } else {
                println!("Matched skills:\n");
                for skill in &matched {
                    print!("  {}", skill.name);
                    if let Some(desc) = &skill.description {
                        print!(" - {}", desc);
                    }
                    println!();
                }
            }
        }
        SkillsAction::Catalog { refresh, query } => {
            let catalog = if refresh {
                println!("Fetching fresh catalog from skills.sh...");
                tools::refresh_skills_catalog()
            } else {
                tools::get_skills_catalog()
            };

            let filtered: Vec<_> = if let Some(ref q) = query {
                let results = tools::fuzzy_search_catalog(q, &catalog);
                results.into_iter().map(|(_, e)| e).collect()
            } else {
                catalog
            };

            if filtered.is_empty() {
                println!("No skills found.");
            } else {
                println!("Skills catalog ({} skills):\n", filtered.len());
                for (i, entry) in filtered.iter().enumerate().take(50) {
                    let trend = entry.trending_rank
                        .map(|r| format!(" 🔥#{}", r + 1))
                        .unwrap_or_default();
                    print!("  {:>3}. {} ({})  {} installs{}",
                        i + 1, entry.name, entry.repo, entry.installs, trend);
                    if let Some(desc) = &entry.description {
                        print!("\n       {}", desc);
                    }
                    println!();
                }
                if filtered.len() > 50 {
                    println!("\n  ... and {} more", filtered.len() - 50);
                }
                println!("\nInstall: rust-code skills add <owner/repo/skill-name>");
            }
        }
    }
    Ok(())
}

async fn run_mcp_command(action: McpAction) -> Result<()> {
    use tools::mcp::McpManager;

    match action {
        McpAction::Config => {
            let config = McpManager::load_configs();
            if config.mcp_servers.is_empty() {
                println!("No MCP servers configured.");
                println!("\nCreate ~/.mcp.json or .mcp.json with:");
                println!(r#"  {{"mcpServers": {{"name": {{"command": "...", "args": [...]}}}}}}"#);
            } else {
                println!("Configured MCP servers ({}):\n", config.mcp_servers.len());
                for (name, cfg) in &config.mcp_servers {
                    println!("  {} -> {} {}", name, cfg.command, cfg.args.join(" "));
                    if !cfg.env.is_empty() {
                        for (k, _) in &cfg.env {
                            println!("    env: {}=***", k);
                        }
                    }
                }
            }
        }
        McpAction::List => {
            let config = McpManager::load_configs();
            if config.mcp_servers.is_empty() {
                println!("No MCP servers configured.");
                return Ok(());
            }

            println!("Starting MCP servers...\n");
            let manager = McpManager::start_all(&config).await?;

            if manager.server_count() == 0 {
                println!("No servers started successfully.");
                return Ok(());
            }

            println!("Connected to {} server(s), {} total tools:\n",
                manager.server_count(), manager.tool_count());

            for tool in manager.all_tools() {
                print!("  [{}] {}", tool.server_name, tool.tool.name);
                if let Some(desc) = &tool.tool.description {
                    let short = if desc.len() > 80 { &desc[..80] } else { desc };
                    print!(" - {}", short);
                }
                println!();
            }

            manager.shutdown().await;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize file logging (keeps stdout clean for TUI)
    let _log_guard = init_logging();

    // Setup panic hook to restore terminal
    setup_panic_hook();

    // Handle subcommands first (no BAML init needed)
    if let Some(command) = args.command {
        return match command {
            Commands::Skills { action } => run_skills_command(action).await,
            Commands::Mcp { action } => run_mcp_command(action).await,
        };
    }

    // Initialize BAML runtime
    baml_client::init();

    if let Some(prompt) = args.prompt {
        // Single prompt headless mode
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        // Initialize MCP servers
        if let Err(e) = agent.init_mcp().await {
            tracing::warn!("MCP init failed: {}", e);
        }
        if args.resume {
            let _ = agent.load_last_session();
        }
        agent.add_user_message(prompt);

        loop {
            println!("Thinking...");
            let step = agent.step().await?;

            println!("\nAnalysis: {}", step.analysis);
            println!("Plan updates:");
            for p in &step.plan_updates {
                println!(" - {}", p);
            }

            println!("\nAction:");
            println!("{:?}", step.action);

            agent.add_assistant_message(format!(
                "Analysis: {}\nAction: {:?}",
                step.analysis, step.action
            ));

            let is_done = matches!(
                step.action,
                baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
            );

            let result = agent.execute_action(&step.action).await?;
            match result {
                AgentEvent::Message(msg) => {
                    println!("\nTool Result:\n{}", msg);
                    agent.add_user_message(format!("Tool result:\n{}", msg));
                }
                AgentEvent::OpenEditor(path, _) => {
                    println!("\nAction requested opening editor for: {}", path);
                    agent.add_user_message(format!("Tool result:\nUser opened editor for {}", path));
                }
            }

            if is_done {
                break;
            }
            println!("----------------------------------------");
        }
    } else {
        // Interactive TUI mode
        let mut terminal = tui::init()?;
        let mut app = app::App::new();

        let result = app.run(&mut terminal, args.resume).await;

        tui::restore()?;

        if let Err(err) = result {
            println!("Error running TUI: {:?}", err);
        }
    }

    Ok(())
}
