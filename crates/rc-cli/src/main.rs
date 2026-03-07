pub mod agent;
pub mod app;
pub mod baml_client;
pub mod preview;
pub mod tools;
pub mod tui;

use crate::agent::Agent;
use anyhow::Result;
use baml_agent::{LoopConfig, LoopEvent, SgrAgent, run_loop_stream};
use clap::{Parser, Subcommand};

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

    /// Use local Ollama model instead of cloud API
    #[arg(long)]
    local: bool,

    /// Override model name (e.g. --model qwen2.5-coder:32b)
    #[arg(long)]
    model: Option<String>,

    /// Use Codex (ChatGPT Plus/Pro) subscription token as OpenAI backend
    #[arg(long)]
    codex: bool,

    /// Intent mode: auto, ask, build, plan (affects which tools the agent uses)
    #[arg(long, default_value = "auto")]
    intent: String,

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
    /// Set default provider (saved to ~/.rust-code/config.toml)
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Check environment health and fix missing dependencies
    Doctor {
        /// Auto-install missing dependencies
        #[arg(long)]
        fix: bool,
    },
    /// Interactive provider setup wizard
    Setup,
}

#[derive(Subcommand, Debug)]
enum McpAction {
    /// List configured MCP servers and their tools
    List,
    /// Show .mcp.json config
    Config,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Show current config
    Show,
    /// Set default provider: gemini, codex, openai, claude, ollama
    Set {
        /// Provider name
        provider: String,
        /// Optional model override
        #[arg(long)]
        model: Option<String>,
    },
    /// Reset to default (Gemini)
    Reset,
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


/// Resolved provider ready to apply to agent.
struct ProviderSetup {
    client: Option<String>,
    label: Option<String>,
    /// Background proxy handle (kept alive for duration of session)
    _proxy_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Start Codex Responses API proxy and configure env vars for BAML.
async fn start_codex_provider(model_override: Option<String>) -> ProviderSetup {
    match tools::start_codex_proxy().await {
        Ok((port, handle)) => {
            let proxy_url = format!("http://127.0.0.1:{}/v1", port);
            // SAFETY: called before spawning agent threads, single-threaded init
            unsafe { std::env::set_var("CODEX_PROXY_URL", &proxy_url) };
            let client = model_override.unwrap_or_else(|| "CodexProxy".into());
            ProviderSetup {
                label: Some(format!("Codex proxy (:{}, {})", port, client)),
                client: Some(client),
                _proxy_handle: Some(handle),
            }
        }
        Err(e) => {
            eprintln!("Failed to start Codex proxy: {}", e);
            eprintln!("Check ~/.codex/auth.json or run `codex login` first");
            std::process::exit(1);
        }
    }
}

/// Start a CLI tool proxy (claude, gemini, codex CLI subprocess).
async fn start_cli_provider(cli_name: &str, model_override: Option<String>) -> ProviderSetup {
    let provider = match tools::CliProvider::from_name(cli_name) {
        Some(p) => p,
        None => {
            eprintln!("Unknown CLI provider: {}", cli_name);
            std::process::exit(1);
        }
    };

    match tools::start_cli_proxy(provider).await {
        Ok((port, handle)) => {
            let proxy_url = format!("http://127.0.0.1:{}/v1", port);
            // SAFETY: called before spawning agent threads, single-threaded init
            unsafe { std::env::set_var("CLI_PROXY_URL", &proxy_url) };
            let client = model_override.unwrap_or_else(|| "CliProxy".into());
            ProviderSetup {
                label: Some(format!("{} proxy (:{}, {})", provider.display_name(), port, client)),
                client: Some(client),
                _proxy_handle: Some(handle),
            }
        }
        Err(e) => {
            eprintln!("Failed to start {} proxy: {}", provider.display_name(), e);
            std::process::exit(1);
        }
    }
}

/// Resolve provider from CLI flags (override) → config file (default).
async fn resolve_provider_setup(args: &Args) -> ProviderSetup {
    use baml_agent::providers::{self, ProviderAuth};

    // CLI flags take priority
    if args.codex {
        return start_codex_provider(args.model.clone()).await;
    }
    if args.local {
        let model = args.model.clone().unwrap_or_else(|| "OllamaDefault".into());
        return ProviderSetup {
            label: Some(format!("local ({})", model)),
            client: Some(model),
            _proxy_handle: None,
        };
    }
    if let Some(ref model) = args.model {
        return ProviderSetup {
            label: Some(model.clone()),
            client: Some(model.clone()),
            _proxy_handle: None,
        };
    }

    // Fall back to config file
    let cfg = providers::load_config(".rust-code");
    if let Some(ref provider) = cfg.provider {
        if let Some((default_client, auth)) = providers::resolve_provider(provider) {
            let client = cfg.model.unwrap_or_else(|| default_client.to_string());
            match auth {
                ProviderAuth::CodexProxy => {
                    return start_codex_provider(Some(client)).await;
                }
                ProviderAuth::CliProxy(cli_name) => {
                    return start_cli_provider(cli_name, Some(client)).await;
                }
                ProviderAuth::EnvKey(_) if provider == "vertex" => {
                    // Auto-detect VERTEX_PROJECT from gcloud if not set
                    if std::env::var("VERTEX_PROJECT").is_err() {
                        if let Ok(out) = std::process::Command::new("gcloud")
                            .args(["config", "get-value", "project"])
                            .output()
                        {
                            let project = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            if !project.is_empty() && out.status.success() {
                                unsafe { std::env::set_var("VERTEX_PROJECT", &project) };
                            }
                        }
                    }
                }
                ProviderAuth::ClaudeKeychain => {
                    match providers::load_claude_keychain_token() {
                        Ok(token) => {
                            // SAFETY: called before spawning threads
                            unsafe { std::env::set_var("ANTHROPIC_API_KEY", &token) };
                        }
                        Err(e) => {
                            eprintln!("Claude auth failed: {}", e);
                            eprintln!("Run `claude` first to authenticate, or use `config set anthropic` with ANTHROPIC_API_KEY");
                            std::process::exit(1);
                        }
                    }
                }
                _ => {}
            }
            return ProviderSetup {
                label: Some(format!("{} ({})", provider, client)),
                client: Some(client),
                _proxy_handle: None,
            };
        }
    }

    // Default: no override (uses BAML default client)
    ProviderSetup { client: None, label: None, _proxy_handle: None }
}

/// Apply resolved provider to agent.
fn apply_provider(setup: &ProviderSetup, agent: &mut Agent) {
    if let Some(ref client) = setup.client {
        agent.set_client(client);
    }
    if let Some(ref label) = setup.label {
        println!("Provider: {}", label);
    }
}

fn run_config_command(action: ConfigAction) -> Result<()> {
    use baml_agent::providers;

    match action {
        ConfigAction::Show => {
            let cfg = providers::load_config(".rust-code");
            let provider = cfg.provider.as_deref().unwrap_or("gemini (default)");
            let model = cfg.model.as_deref().unwrap_or("(auto)");
            println!("Provider: {}", provider);
            println!("Model:    {}", model);
            println!("\nConfig: ~/.rust-code/config.toml");
            println!("Available: gemini, claude, codex, openai, anthropic, ollama, gemini-cli, codex-cli, claude-cli");
        }
        ConfigAction::Set { provider, model } => {
            if providers::resolve_provider(&provider).is_none() {
                eprintln!("Unknown provider: {}", provider);
                eprintln!("Available: gemini, claude, codex, openai, anthropic, ollama, gemini-cli, codex-cli, claude-cli");
                std::process::exit(1);
            }
            let cfg = providers::UserConfig {
                provider: Some(provider.clone()),
                model,
            };
            providers::save_config(".rust-code", &cfg).map_err(|e| anyhow::anyhow!(e))?;
            println!("Default provider set to: {}", provider);
            println!("Saved to ~/.rust-code/config.toml");
        }
        ConfigAction::Reset => {
            let cfg = providers::UserConfig::default();
            providers::save_config(".rust-code", &cfg).map_err(|e| anyhow::anyhow!(e))?;
            println!("Config reset to defaults (Gemini)");
        }
    }
    Ok(())
}

/// Check if gcloud Application Default Credentials exist.
fn check_gcloud_adc() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let adc_path = std::path::Path::new(&home)
        .join(".config/gcloud/application_default_credentials.json");
    adc_path.exists()
}

async fn run_setup() -> Result<()> {
    use baml_agent::providers::{self, ProviderAuth};
    use std::io::{self, BufRead, Write};

    println!("rust-code setup\n");
    println!("Choose your LLM provider:\n");

    // Group providers for display
    struct ProviderOption {
        name: &'static str,
        desc: &'static str,
        auth_hint: &'static str,
    }

    let options = [
        ProviderOption {
            name: "gemini",
            desc: "Google Gemini via API key",
            auth_hint: "GEMINI_API_KEY",
        },
        ProviderOption {
            name: "vertex",
            desc: "Google Gemini via Vertex AI (Google Cloud)",
            auth_hint: "GOOGLE_APPLICATION_CREDENTIALS",
        },
        ProviderOption {
            name: "claude",
            desc: "Claude via Claude Code auth (macOS Keychain)",
            auth_hint: "run `claude` first",
        },
        ProviderOption {
            name: "anthropic",
            desc: "Claude via API key",
            auth_hint: "ANTHROPIC_API_KEY",
        },
        ProviderOption {
            name: "openai",
            desc: "OpenAI (API key)",
            auth_hint: "OPENAI_API_KEY",
        },
        ProviderOption {
            name: "codex",
            desc: "ChatGPT Plus/Pro subscription",
            auth_hint: "run `codex login` first",
        },
        ProviderOption {
            name: "ollama",
            desc: "Local models via Ollama (free)",
            auth_hint: "install ollama",
        },
        ProviderOption {
            name: "claude-cli",
            desc: "Claude via `claude` CLI subprocess",
            auth_hint: "install claude CLI",
        },
        ProviderOption {
            name: "gemini-cli",
            desc: "Gemini via `gemini` CLI subprocess",
            auth_hint: "install gemini CLI",
        },
    ];

    // Check which have auth ready
    for (i, opt) in options.iter().enumerate() {
        let status = match opt.name {
            "gemini" => {
                if std::env::var("GEMINI_API_KEY").is_ok() {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "vertex" => {
                if std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
                    || std::env::var("VERTEX_PROJECT").is_ok()
                    || check_gcloud_adc()
                {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "anthropic" => {
                if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "openai" => {
                if std::env::var("OPENAI_API_KEY").is_ok() {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "claude" => {
                if providers::load_claude_keychain_token().is_ok() {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "codex" | "chatgpt" => {
                let auth_path = std::env::var("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".codex/auth.json"))
                    .unwrap_or_default();
                if auth_path.exists() {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            "ollama" => {
                if std::process::Command::new("ollama")
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            cli if cli.ends_with("-cli") => {
                let cmd = cli.strip_suffix("-cli").unwrap_or(cli);
                if std::process::Command::new(cmd)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[33m·\x1b[0m"
                }
            }
            _ => "\x1b[33m·\x1b[0m",
        };
        println!(
            "  {} {:>2}) \x1b[1m{}\x1b[0m — {} [{}]",
            status,
            i + 1,
            opt.name,
            opt.desc,
            opt.auth_hint
        );
    }

    println!("\n\x1b[32m✓\x1b[0m = auth ready, \x1b[33m·\x1b[0m = needs setup\n");
    print!("Enter number (1-{}): ", options.len());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    let choice: usize = input
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid number"))?;

    if choice == 0 || choice > options.len() {
        anyhow::bail!("Invalid choice: {}", choice);
    }

    let selected = &options[choice - 1];
    let provider_name = selected.name;

    // Validate auth
    if let Some((_, auth)) = providers::resolve_provider(provider_name) {
        let ok = match &auth {
            ProviderAuth::EnvKey(var) => {
                // Vertex AI also works with ADC (gcloud auth) or VERTEX_PROJECT
                let found = std::env::var(var).is_ok()
                    || (provider_name == "vertex"
                        && (std::env::var("VERTEX_PROJECT").is_ok()
                            || check_gcloud_adc()));
                if found {
                    true
                } else {
                    if provider_name == "vertex" {
                        eprintln!("\n\x1b[33mWarning:\x1b[0m No Google Cloud auth found.");
                        eprintln!("  Option 1: gcloud auth application-default login");
                        eprintln!("  Option 2: export GOOGLE_APPLICATION_CREDENTIALS=/path/to/key.json");
                        eprintln!("  Option 3: export VERTEX_PROJECT=your-project-id");
                    } else {
                        eprintln!(
                            "\n\x1b[33mWarning:\x1b[0m {} not set. Set it in your shell profile:",
                            var
                        );
                        eprintln!("  export {}=\"your-api-key\"", var);
                    }
                    false
                }
            }
            ProviderAuth::ClaudeKeychain => {
                if providers::load_claude_keychain_token().is_ok() {
                    true
                } else {
                    eprintln!("\n\x1b[33mWarning:\x1b[0m Claude auth not found.");
                    eprintln!("  Run `claude` first to authenticate via OAuth.");
                    false
                }
            }
            ProviderAuth::CodexProxy => {
                let auth_path = std::env::var("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".codex/auth.json"))
                    .unwrap_or_default();
                if auth_path.exists() {
                    true
                } else {
                    eprintln!("\n\x1b[33mWarning:\x1b[0m Codex auth not found.");
                    eprintln!("  Run `codex login` first.");
                    false
                }
            }
            ProviderAuth::CliProxy(cli_name) => {
                if std::process::Command::new(cli_name)
                    .arg("--version")
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    true
                } else {
                    eprintln!(
                        "\n\x1b[33mWarning:\x1b[0m `{}` CLI not found in PATH.",
                        cli_name
                    );
                    false
                }
            }
            ProviderAuth::None => true,
        };

        if !ok {
            print!("\nSave anyway? (y/N): ");
            io::stdout().flush()?;
            let mut confirm = String::new();
            io::stdin().lock().read_line(&mut confirm)?;
            if !confirm.trim().eq_ignore_ascii_case("y") {
                println!("Setup cancelled. Run `rust-code setup` again when ready.");
                return Ok(());
            }
        }
    }

    // Save config
    let cfg = providers::UserConfig {
        provider: Some(provider_name.to_string()),
        model: None,
    };
    providers::save_config(".rust-code", &cfg).map_err(|e| anyhow::anyhow!(e))?;

    println!(
        "\n\x1b[32m✓\x1b[0m Provider set to: \x1b[1m{}\x1b[0m",
        provider_name
    );
    println!("  Saved to ~/.rust-code/config.toml");
    println!("  Change later: rust-code config set <provider>");

    Ok(())
}

fn init_telemetry_headless() -> baml_agent::TelemetryGuard {
    baml_agent::init_telemetry(".rust-code", "rust-code")
}

fn init_telemetry_tui() -> baml_agent_tui::TuiTelemetryGuard {
    baml_agent_tui::init_tui_telemetry(".rust-code", "rust-code")
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
        SkillsAction::Remove { name } => match tools::remove_skill(&name) {
            Ok(()) => println!("Removed skill '{}'.", name),
            Err(e) => {
                eprintln!("Failed to remove '{}': {}", name, e);
                std::process::exit(1);
            }
        },
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
                    println!(
                        "  {} ({})  {} installs",
                        entry.name, entry.repo, entry.installs
                    );
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
                    let trend = entry
                        .trending_rank
                        .map(|r| format!(" 🔥#{}", r + 1))
                        .unwrap_or_default();
                    print!(
                        "  {:>3}. {} ({})  {} installs{}",
                        i + 1,
                        entry.name,
                        entry.repo,
                        entry.installs,
                        trend
                    );
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

            println!(
                "Connected to {} server(s), {} total tools:\n",
                manager.server_count(),
                manager.tool_count()
            );

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
                let tag = if check.required {
                    "required"
                } else {
                    "optional"
                };
                println!(
                    "  \x1b[32m✓\x1b[0m {} — {} [{}]",
                    check.name,
                    ver_line.trim(),
                    tag
                );
                ok_count += 1;
            }
            _ => {
                let tag = if check.required {
                    "\x1b[31m✗\x1b[0m"
                } else {
                    "\x1b[33m-\x1b[0m"
                };
                let install = if is_mac {
                    check.install_brew
                } else {
                    check.install_other
                };
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
        println!(
            "  \x1b[32m✓\x1b[0m .mcp.json — {} server(s) configured",
            config.mcp_servers.len()
        );
    } else {
        println!("  \x1b[33m-\x1b[0m .mcp.json — not found (optional, for MCP tools)");
    }

    // Skills
    let skills = tools::collect_installed_skills();
    println!("  \x1b[32m✓\x1b[0m skills — {} installed", skills.len());

    // Provider config
    let cfg = baml_agent::providers::load_config(".rust-code");
    let provider_name = cfg.provider.as_deref().unwrap_or("(not set)");
    println!("  \x1b[36mℹ\x1b[0m provider — {}", provider_name);

    // LLM credentials — check based on configured provider
    let has_gemini = std::env::var("GEMINI_API_KEY").is_ok();
    let has_vertex = std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok()
        || std::env::var("VERTEX_PROJECT").is_ok()
        || check_gcloud_adc();
    let has_anthropic = std::env::var("ANTHROPIC_API_KEY").is_ok();
    let has_openai = std::env::var("OPENAI_API_KEY").is_ok();
    let has_claude_keychain = baml_agent::providers::load_claude_keychain_token().is_ok();

    match cfg.provider.as_deref() {
        Some("gemini") => {
            if has_gemini {
                println!("  \x1b[32m✓\x1b[0m LLM auth — GEMINI_API_KEY set");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — GEMINI_API_KEY not set");
            }
        }
        Some("vertex" | "vertex-ai") => {
            if has_vertex {
                let method = if std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok() {
                    "service account key"
                } else if std::env::var("VERTEX_PROJECT").is_ok() {
                    "VERTEX_PROJECT"
                } else {
                    "gcloud ADC"
                };
                println!("  \x1b[32m✓\x1b[0m LLM auth — Vertex AI via {}", method);
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — no Google Cloud credentials");
                println!("    fix: gcloud auth application-default login");
            }
        }
        Some("claude") => {
            if has_claude_keychain {
                println!("  \x1b[32m✓\x1b[0m LLM auth — Claude Keychain token found");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — no Claude Keychain token");
                println!("    fix: run `claude` to authenticate");
            }
        }
        Some("anthropic") => {
            if has_anthropic {
                println!("  \x1b[32m✓\x1b[0m LLM auth — ANTHROPIC_API_KEY set");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — ANTHROPIC_API_KEY not set");
            }
        }
        Some("openai") => {
            if has_openai {
                println!("  \x1b[32m✓\x1b[0m LLM auth — OPENAI_API_KEY set");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — OPENAI_API_KEY not set");
            }
        }
        Some("codex" | "chatgpt") => {
            let auth_path = std::path::Path::new(&home).join(".codex/auth.json");
            if auth_path.exists() {
                println!("  \x1b[32m✓\x1b[0m LLM auth — Codex auth.json found");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — run `codex login` first");
            }
        }
        Some("ollama" | "local") => {
            if Cmd::new("ollama").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
                println!("  \x1b[32m✓\x1b[0m LLM auth — ollama installed (no auth needed)");
            } else {
                println!("  \x1b[31m✗\x1b[0m LLM auth — ollama not installed");
            }
        }
        None => {
            if has_gemini || has_vertex {
                println!("  \x1b[32m✓\x1b[0m LLM auth — credentials found (run `rust-code setup` to set provider)");
            } else {
                println!("  \x1b[33m-\x1b[0m LLM auth — no provider configured, run `rust-code setup`");
            }
        }
        Some(other) => {
            println!("  \x1b[33m-\x1b[0m LLM auth — unknown provider '{}', can't verify", other);
        }
    }

    // Summary
    println!(
        "\n{}/{} checks passed, {} missing\n",
        ok_count,
        checks.len(),
        missing.len()
    );

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
                Ok(s) => println!(
                    "    \x1b[31m✗\x1b[0m failed (exit {})",
                    s.code().unwrap_or(-1)
                ),
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
        matches
            .into_iter()
            .map(|(score, m)| (Some(score), m))
            .collect::<Vec<_>>()
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

        let topic = if meta.topic.is_empty() {
            "(no topic)"
        } else {
            &meta.topic
        };
        if let Some(s) = score {
            println!(
                "  \x1b[1m{}\x1b[0m  [score:{}] {} msgs, {}",
                topic, s, meta.message_count, age_str
            );
        } else {
            println!(
                "  \x1b[1m{}\x1b[0m  {} msgs, {}",
                topic, meta.message_count, age_str
            );
        }
        println!("    {}", meta.path.display());
    }

    println!("\nResume: rust-code -r \"<topic>\" -p \"continue...\"");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup panic hook to restore terminal
    setup_panic_hook();

    // Handle subcommands first (no BAML init needed)
    if let Some(command) = args.command {
        return match command {
            Commands::Skills { action } => run_skills_command(action).await,
            Commands::Mcp { action } => run_mcp_command(action).await,
            Commands::Sessions { query } => run_sessions_command(query),
            Commands::Config { action } => run_config_command(action),
            Commands::Doctor { fix } => run_doctor(fix).await,
            Commands::Setup => run_setup().await,
        };
    }

    // Initialize OTEL-aware structured telemetry (JSONL + span context)
    // TUI mode redirects stderr to file (prevents BAML output from corrupting ratatui)
    let is_headless = args.prompt.is_some();
    let _telemetry: Box<dyn std::any::Any> = if is_headless {
        Box::new(init_telemetry_headless())
    } else {
        Box::new(init_telemetry_tui())
    };

    // Initialize BAML runtime
    baml_client::init();

    // Auto-setup on first run if no config and no CLI flags
    if !args.codex && !args.local && args.model.is_none() {
        let cfg = baml_agent::providers::load_config(".rust-code");
        if cfg.provider.is_none() && std::env::var("GEMINI_API_KEY").is_err() {
            eprintln!("First run detected — checking environment...\n");
            let _ = run_doctor(false).await;
            println!();
            if let Err(e) = run_setup().await {
                eprintln!("Setup failed: {}", e);
                std::process::exit(1);
            }
            println!();
        }
    }

    // Resolve provider: CLI flags override config file
    let provider_setup = resolve_provider_setup(&args).await;

    if let Some(prompt) = args.prompt {
        // Single prompt headless mode — fresh session by default
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        apply_provider(&provider_setup, &mut agent);
        // Set intent from CLI flag
        agent.intent = match args.intent.as_str() {
            "ask" => baml_agent::Intent::Ask,
            "build" => baml_agent::Intent::Build,
            "plan" => baml_agent::Intent::Plan,
            _ => baml_agent::Intent::Auto,
        };
        if agent.intent != baml_agent::Intent::Auto {
            println!("Intent: {:?}", agent.intent);
        }
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

        let config = LoopConfig {
            max_steps: 50,
            loop_abort_threshold: 6,
        };

        // Extract session for run_loop_stream (needs &Agent + &mut Session separately)
        let mut session = std::mem::replace(
            agent.session_mut(),
            baml_agent::Session::new(".rust-code-tmp", 60).expect("tmp session dir"),
        );

        use std::io::Write as _;
        let result = run_loop_stream(&agent, &mut session, &config, |event| match event {
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
                eprintln!(
                    "[ERR] Agent stuck in loop after {} identical actions — aborting",
                    n
                );
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
        })
        .await;

        // Restore session
        *agent.session_mut() = session;

        // Show cost summary
        let cost = crate::tools::cost::session_stats();
        if cost.steps > 0 {
            eprintln!("\n\x1b[2m{}\x1b[0m", cost.status_line());
        }

        if let Err(e) = result {
            eprintln!("Agent error: {}", e);
        }
    } else {
        // Interactive TUI mode
        let mut terminal = tui::init()?;
        let mut app = app::App::new();

        // Apply provider from config/flags
        if let Some(ref client) = provider_setup.client {
            app.set_client_override(client);
        }

        let result = app
            .run(
                &mut terminal,
                args.resume.as_deref(),
                args.session.as_deref(),
            )
            .await;

        tui::restore()?;

        if let Err(err) = result {
            println!("Error running TUI: {:?}", err);
        }
    }

    Ok(())
}
