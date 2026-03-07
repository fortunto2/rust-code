pub mod tui;
pub mod app;
pub mod preview;
pub mod agent;
pub mod tools;
pub mod baml_client;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crate::agent::Agent;
use baml_agent::{LoopConfig, LoopEvent, SgrAgent, run_loop_stream};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    prompt: Option<String>,

    /// Resume last session, or fuzzy-search by topic (e.g. --resume "parser bug")
    #[arg(short, long, num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,

    /// Resume a specific session by file path (e.g. .rust-code/session_123.jsonl)
    #[arg(short, long)]
    session: Option<String>,

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
    /// List or search past sessions
    Sessions {
        /// Fuzzy search query (omit to list all)
        query: Option<String>,
    },
    /// Check environment health and fix missing dependencies
    Doctor {
        /// Auto-install missing dependencies
        #[arg(long)]
        fix: bool,
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

fn init_logging() -> impl Drop {
    baml_agent::init_logging(".rust-code", "rust-code")
}

fn setup_panic_hook() {
    tui::setup_panic_hook();
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

async fn run_doctor(fix: bool) -> Result<()> {
    use std::process::Command as Cmd;

    struct Check {
        name: &'static str,
        cmd: &'static str,
        args: &'static [&'static str],
        install_brew: &'static str,
        install_other: &'static str,
        required: bool,
    }

    let checks = [
        Check {
            name: "tmux",
            cmd: "tmux",
            args: &["-V"],
            install_brew: "brew install tmux",
            install_other: "apt install tmux",
            required: true,
        },
        Check {
            name: "ripgrep (rg)",
            cmd: "rg",
            args: &["--version"],
            install_brew: "brew install ripgrep",
            install_other: "cargo install ripgrep",
            required: true,
        },
        Check {
            name: "git",
            cmd: "git",
            args: &["--version"],
            install_brew: "brew install git",
            install_other: "apt install git",
            required: true,
        },
        Check {
            name: "python3",
            cmd: "python3",
            args: &["--version"],
            install_brew: "brew install python3",
            install_other: "apt install python3",
            required: false,
        },
        Check {
            name: "node",
            cmd: "node",
            args: &["--version"],
            install_brew: "brew install node",
            install_other: "curl -fsSL https://fnm.vercel.app/install | bash",
            required: false,
        },
        Check {
            name: "cargo",
            cmd: "cargo",
            args: &["--version"],
            install_brew: "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
            install_other: "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
            required: false,
        },
    ];

    let is_mac = cfg!(target_os = "macos");
    let mut missing: Vec<(&str, String)> = Vec::new();
    let mut ok_count = 0;

    println!("rust-code doctor\n");

    // Tool checks
    for check in &checks {
        match Cmd::new(check.cmd).args(check.args).output() {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout);
                let ver_line = ver.lines().next().unwrap_or("ok");
                let tag = if check.required { "required" } else { "optional" };
                println!("  \x1b[32m✓\x1b[0m {} — {} [{}]", check.name, ver_line.trim(), tag);
                ok_count += 1;
            }
            _ => {
                let tag = if check.required { "\x1b[31m✗\x1b[0m" } else { "\x1b[33m-\x1b[0m" };
                let install = if is_mac { check.install_brew } else { check.install_other };
                println!("  {} {} — missing [fix: {}]", tag, check.name, install);
                missing.push((check.name, install.to_string()));
            }
        }
    }

    // Config checks
    println!();
    let home = std::env::var("HOME").unwrap_or_default();

    // .mcp.json
    let mcp_global = std::path::Path::new(&home).join(".mcp.json");
    let mcp_local = std::path::Path::new(".mcp.json");
    if mcp_global.exists() || mcp_local.exists() {
        let config = tools::mcp::McpManager::load_configs();
        println!("  \x1b[32m✓\x1b[0m .mcp.json — {} server(s) configured", config.mcp_servers.len());
    } else {
        println!("  \x1b[33m-\x1b[0m .mcp.json — not found (optional, for MCP tools)");
    }

    // Skills
    let skills = tools::collect_installed_skills();
    println!("  \x1b[32m✓\x1b[0m skills — {} installed", skills.len());

    // BAML / API key
    let has_vertex = std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
        || std::env::var("VERTEX_PROJECT").is_ok();
    let has_gemini = std::env::var("GEMINI_API_KEY").is_ok();
    if has_vertex || has_gemini {
        println!("  \x1b[32m✓\x1b[0m LLM credentials — configured");
    } else {
        println!("  \x1b[33m-\x1b[0m LLM credentials — no VERTEX_PROJECT or GEMINI_API_KEY found");
    }

    // Summary
    println!("\n{}/{} checks passed, {} missing\n",
        ok_count, checks.len(), missing.len());

    if missing.is_empty() {
        println!("\x1b[32mAll good!\x1b[0m rust-code is ready.");
        return Ok(());
    }

    if fix {
        println!("Installing missing dependencies...\n");
        for (name, cmd) in &missing {
            println!("  → {} ...", name);
            let status = Cmd::new("sh").arg("-c").arg(cmd).status();
            match status {
                Ok(s) if s.success() => println!("    \x1b[32m✓\x1b[0m installed"),
                Ok(s) => println!("    \x1b[31m✗\x1b[0m failed (exit {})", s.code().unwrap_or(-1)),
                Err(e) => println!("    \x1b[31m✗\x1b[0m error: {}", e),
            }
        }
        println!("\nRe-run `rust-code doctor` to verify.");
    } else {
        println!("Run \x1b[1mrust-code doctor --fix\x1b[0m to install missing dependencies.");
        // Print one-liner
        let cmds: Vec<&str> = missing.iter().map(|(_, c)| c.as_str()).collect();
        println!("Or manually: {}", cmds.join(" && "));
    }

    Ok(())
}

fn run_sessions_command(query: Option<String>) -> Result<()> {
    let sessions = if let Some(ref q) = query {
        let matches = baml_agent::search_sessions(".rust-code", q);
        if matches.is_empty() {
            println!("No sessions matching '{}'.", q);
            return Ok(());
        }
        println!("Sessions matching '{}' ({}):\n", q, matches.len());
        matches.into_iter().map(|(score, m)| (Some(score), m)).collect::<Vec<_>>()
    } else {
        let all = baml_agent::list_sessions(".rust-code");
        if all.is_empty() {
            println!("No sessions found in .rust-code/");
            return Ok(());
        }
        println!("Sessions ({}):\n", all.len());
        all.into_iter().map(|m| (None, m)).collect::<Vec<_>>()
    };

    for (score, meta) in &sessions {
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(meta.created);
        let age_str = if age < 3600 {
            format!("{}m ago", age / 60)
        } else if age < 86400 {
            format!("{}h ago", age / 3600)
        } else {
            format!("{}d ago", age / 86400)
        };

        let topic = if meta.topic.is_empty() { "(no topic)" } else { &meta.topic };
        if let Some(s) = score {
            println!("  \x1b[1m{}\x1b[0m  [score:{}] {} msgs, {}", topic, s, meta.message_count, age_str);
        } else {
            println!("  \x1b[1m{}\x1b[0m  {} msgs, {}", topic, meta.message_count, age_str);
        }
        println!("    {}", meta.path.display());
    }

    println!("\nResume: rust-code -r \"<topic>\" -p \"continue...\"");
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
            Commands::Sessions { query } => run_sessions_command(query),
            Commands::Doctor { fix } => run_doctor(fix).await,
        };
    }

    // Initialize BAML runtime
    baml_client::init();

    if let Some(prompt) = args.prompt {
        // Single prompt headless mode — fresh session by default
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        // Initialize MCP servers
        if let Err(e) = agent.init_mcp().await {
            tracing::warn!("MCP init failed: {}", e);
        }
        // Only resume if explicitly requested via --session or --resume
        if let Some(session_path) = &args.session {
            let _ = agent.load_session_file(std::path::Path::new(session_path));
        } else if let Some(query) = &args.resume {
            if query.is_empty() {
                let _ = agent.load_last_session();
            } else {
                // Fuzzy search for matching session
                let matches = baml_agent::search_sessions(".rust-code", query);
                if let Some((score, meta)) = matches.first() {
                    println!("Resuming: \"{}\" (score: {})", meta.topic, score);
                    let _ = agent.load_session_file(&meta.path);
                } else {
                    println!("No session matching '{}', starting fresh.", query);
                }
            }
        }
        agent.add_user_message(&prompt);

        let config = LoopConfig { max_steps: 50, loop_abort_threshold: 6 };

        // Extract session for run_loop_stream (needs &Agent + &mut Session separately)
        let mut session = std::mem::replace(
            agent.session_mut(),
            baml_agent::Session::new(".rust-code-tmp", 60),
        );

        use std::io::Write as _;
        let result = run_loop_stream(&agent, &mut session, &config, |event| {
            match event {
                LoopEvent::StepStart(n) => {
                    print!("\n[Step {}] Thinking...", n);
                    std::io::stdout().flush().ok();
                }
                LoopEvent::Decision { situation, task } => {
                    print!("\r\x1b[K");
                    println!("\x1b[2mSituation:\x1b[0m {}", situation);
                    if !task.is_empty() {
                        println!("Task:");
                        for t in task {
                            println!(" - {}", t);
                        }
                    }
                }
                LoopEvent::Completed => {
                    println!("\n[DONE] Task completed.");
                }
                LoopEvent::ActionStart(action) => {
                    println!("\nAction: {}", Agent::action_signature(action));
                }
                LoopEvent::ActionDone(result) => {
                    println!("\nTool Result:\n{}", result.output);
                }
                LoopEvent::LoopWarning(n) => {
                    println!("[WARN] Loop detected — {} repeats", n);
                }
                LoopEvent::LoopAbort(n) => {
                    eprintln!("[ERR] Agent stuck in loop after {} identical actions — aborting", n);
                }
                LoopEvent::Trimmed(n) => {
                    println!("[TRIM] Removed {} messages", n);
                }
                LoopEvent::MaxStepsReached(n) => {
                    eprintln!("[ERR] Max steps ({}) reached", n);
                }
                LoopEvent::StreamToken(token) => {
                    print!("{}", token);
                    std::io::stdout().flush().ok();
                }
            }
        }).await;

        // Restore session
        *agent.session_mut() = session;

        if let Err(e) = result {
            eprintln!("Agent error: {}", e);
        }
    } else {
        // Interactive TUI mode
        let mut terminal = tui::init()?;
        let mut app = app::App::new();

        let result = app.run(&mut terminal, args.resume.as_deref(), args.session.as_deref()).await;

        tui::restore()?;

        if let Err(err) = result {
            println!("Error running TUI: {:?}", err);
        }
    }

    Ok(())
}
