pub mod agent;
pub mod app;
pub mod backend;
pub mod config;
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

    /// Use SGR backend (pure Rust, no BAML runtime) instead of BAML
    #[arg(long)]
    sgr: bool,

    /// Use Gemini CLI as LLM backend (subscription, no API key needed)
    /// Note: Gemini CLI adds its own system prompt, best for simple tasks.
    /// For multi-step agent work, prefer --sgr with GEMINI_API_KEY.
    #[arg(long)]
    gemini_cli: bool,

    /// Intent mode: auto, ask, build, plan (affects which tools the agent uses)
    #[arg(long, default_value = "auto")]
    intent: String,

    /// Working directory for headless mode (default: current directory)
    #[arg(long)]
    cwd: Option<String>,

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
    /// Manage project tasks (.tasks/*.md)
    Task {
        #[command(subcommand)]
        action: TaskAction,
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

#[derive(Subcommand, Debug)]
enum TaskAction {
    /// List all tasks
    List {
        /// Filter by status: todo, in_progress, blocked, done
        #[arg(long)]
        status: Option<String>,
    },
    /// Show task details
    Show {
        /// Task ID
        id: u16,
    },
    /// Create a new task
    Create {
        /// Task title
        title: String,
        /// Priority: low, medium, high
        #[arg(short, long, default_value = "medium")]
        priority: String,
    },
    /// Mark a task as done
    Done {
        /// Task ID
        id: u16,
    },
    /// Update task status
    Update {
        /// Task ID
        id: u16,
        /// New status: todo, in_progress, blocked, done
        #[arg(short, long)]
        status: String,
    },
}

/// Resolved provider ready to apply to agent.
struct ProviderSetup {
    label: Option<String>,
    /// SGR provider.
    provider: Option<backend::SgrProvider>,
    /// Background proxy handle (kept alive for duration of session)
    _proxy_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Start Codex Responses API proxy and configure as OpenAI-compatible provider.
async fn start_codex_provider(model_override: Option<String>) -> ProviderSetup {
    match tools::start_codex_proxy().await {
        Ok((port, handle)) => {
            let proxy_url = format!("http://127.0.0.1:{}/v1", port);
            let model = model_override.unwrap_or_else(|| "codex".into());
            ProviderSetup {
                label: Some(format!("Codex proxy (:{}, {})", port, model)),
                provider: Some(backend::SgrProvider::OpenAI {
                    api_key: "proxy".into(),
                    model,
                    base_url: Some(proxy_url),
                }),
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
    let cli_provider = match tools::CliProvider::from_name(cli_name) {
        Some(p) => p,
        None => {
            eprintln!("Unknown CLI provider: {}", cli_name);
            std::process::exit(1);
        }
    };

    match tools::start_cli_proxy(cli_provider).await {
        Ok((port, handle)) => {
            let proxy_url = format!("http://127.0.0.1:{}/v1", port);
            let model = model_override.unwrap_or_else(|| "cli-proxy".into());
            ProviderSetup {
                label: Some(format!(
                    "{} proxy (:{}, {})",
                    cli_provider.display_name(),
                    port,
                    model
                )),
                provider: Some(backend::SgrProvider::OpenAI {
                    api_key: "proxy".into(),
                    model,
                    base_url: Some(proxy_url),
                }),
                _proxy_handle: Some(handle),
            }
        }
        Err(e) => {
            eprintln!(
                "Failed to start {} proxy: {}",
                cli_provider.display_name(),
                e
            );
            std::process::exit(1);
        }
    }
}

/// Resolve provider from CLI flags (override) → config file (default) → auto-detect.
async fn resolve_provider_setup(args: &Args) -> ProviderSetup {
    use baml_agent::providers::{self, ProviderAuth};

    // --sgr flag or --gemini-cli: resolve SGR provider from env/flags
    if args.sgr || args.gemini_cli {
        let (provider, proxy_handle) = resolve_sgr_provider(args).await;
        let label = provider_label(&provider);
        return ProviderSetup {
            label: Some(label),
            provider: Some(provider),
            _proxy_handle: proxy_handle,
        };
    }

    // CLI flags take priority
    if args.codex {
        return start_codex_provider(args.model.clone()).await;
    }
    if args.local {
        let model = args.model.clone().unwrap_or_else(|| "llama3".into());
        return ProviderSetup {
            label: Some(format!("local ({})", model)),
            provider: Some(backend::SgrProvider::OpenAI {
                api_key: "ollama".into(),
                model,
                base_url: Some("http://localhost:11434/v1".into()),
            }),
            _proxy_handle: None,
        };
    }

    // Fall back to config file
    let cfg = providers::load_config(".rust-code");
    if let Some(ref prov_name) = cfg.provider {
        if let Some((_default_client, auth)) = providers::resolve_provider(prov_name) {
            let model = cfg.model.clone();
            match auth {
                ProviderAuth::CodexProxy => {
                    return start_codex_provider(model).await;
                }
                ProviderAuth::CliProxy(cli_name) => {
                    return start_cli_provider(cli_name, model).await;
                }
                ProviderAuth::EnvKey(key) if prov_name == "vertex" => {
                    // Auto-detect VERTEX_PROJECT from gcloud if not set
                    let project = std::env::var("VERTEX_PROJECT").ok().or_else(|| {
                        std::process::Command::new("gcloud")
                            .args(["config", "get-value", "project"])
                            .output()
                            .ok()
                            .filter(|o| o.status.success())
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                            .filter(|s| !s.is_empty())
                    });
                    if let Some(project) = project {
                        let m = model.unwrap_or_else(|| "gemini-3.1-pro-preview".into());
                        let location = std::env::var("VERTEX_LOCATION")
                            .ok()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "global".into());
                        return ProviderSetup {
                            label: Some(format!("Vertex ({}, {}, {})", m, project, location)),
                            provider: Some(backend::SgrProvider::Vertex {
                                project_id: project,
                                model: m,
                                location,
                            }),
                            _proxy_handle: None,
                        };
                    }
                }
                ProviderAuth::EnvKey(key) if prov_name == "gemini" || prov_name == "google" => {
                    if let Ok(api_key) = std::env::var(&key) {
                        let m = model.unwrap_or_else(|| "gemini-3.1-pro-preview".into());
                        return ProviderSetup {
                            label: Some(format!("Gemini ({})", m)),
                            provider: Some(backend::SgrProvider::Gemini { api_key, model: m }),
                            _proxy_handle: None,
                        };
                    }
                }
                ProviderAuth::EnvKey(key) if prov_name == "openai" => {
                    if let Ok(api_key) = std::env::var(&key) {
                        let m = model.unwrap_or_else(|| "gpt-4o".into());
                        return ProviderSetup {
                            label: Some(format!("OpenAI ({})", m)),
                            provider: Some(backend::SgrProvider::OpenAI {
                                api_key,
                                model: m,
                                base_url: None,
                            }),
                            _proxy_handle: None,
                        };
                    }
                }
                ProviderAuth::ClaudeKeychain => match providers::load_claude_keychain_token() {
                    Ok(token) => {
                        let m = model.unwrap_or_else(|| "claude-sonnet-4-20250514".into());
                        return ProviderSetup {
                            label: Some(format!("Anthropic ({})", m)),
                            provider: Some(backend::SgrProvider::OpenAI {
                                api_key: token,
                                model: m,
                                base_url: Some("https://api.anthropic.com/v1".into()),
                            }),
                            _proxy_handle: None,
                        };
                    }
                    Err(e) => {
                        eprintln!("Claude auth failed: {}", e);
                    }
                },
                _ => {}
            }
        }
    }

    // Auto-detect: try GEMINI_API_KEY
    if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
        let model = args
            .model
            .clone()
            .unwrap_or_else(|| "gemini-3.1-pro-preview".into());
        return ProviderSetup {
            label: Some(format!("Gemini ({})", model)),
            provider: Some(backend::SgrProvider::Gemini { api_key, model }),
            _proxy_handle: None,
        };
    }

    // No provider found
    ProviderSetup {
        label: None,
        provider: None,
        _proxy_handle: None,
    }
}

fn provider_label(provider: &backend::SgrProvider) -> String {
    match provider {
        backend::SgrProvider::Gemini { model, .. } => format!("Gemini ({})", model),
        backend::SgrProvider::OpenAI {
            model, base_url, ..
        } => {
            if base_url.is_some() {
                format!("OpenAI-compat ({})", model)
            } else {
                format!("OpenAI ({})", model)
            }
        }
        backend::SgrProvider::Vertex {
            model, project_id, ..
        } => {
            format!("Vertex ({}, {})", model, project_id)
        }
        backend::SgrProvider::GeminiCli { model, sandbox } => {
            let m = model.as_deref().unwrap_or("default");
            if *sandbox {
                format!("Gemini CLI ({}, sandbox)", m)
            } else {
                format!("Gemini CLI ({})", m)
            }
        }
    }
}

/// Resolve SGR provider from env vars and CLI args.
async fn resolve_sgr_provider(
    args: &Args,
) -> (backend::SgrProvider, Option<tokio::task::JoinHandle<()>>) {
    // --gemini-cli flag: force CLI proxy (subscription, no API key)
    if args.gemini_cli {
        return start_sgr_gemini_cli(args).await;
    }

    // Check GEMINI_API_KEY first (default SGR provider)
    if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
        if !api_key.is_empty() {
            let model = args
                .model
                .clone()
                .unwrap_or_else(|| "gemini-3.1-pro-preview".into());
            return (backend::SgrProvider::Gemini { api_key, model }, None);
        }
    }

    // OpenAI / OpenRouter
    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        let model = args.model.clone().unwrap_or_else(|| "gpt-4o".into());
        return (
            backend::SgrProvider::OpenAI {
                api_key,
                model,
                base_url: std::env::var("OPENAI_BASE_URL").ok(),
            },
            None,
        );
    }

    if let Ok(api_key) = std::env::var("OPENROUTER_API_KEY") {
        let model = args
            .model
            .clone()
            .unwrap_or_else(|| "google/gemini-2.5-flash".into());
        return (
            backend::SgrProvider::OpenAI {
                api_key,
                model,
                base_url: Some("https://openrouter.ai/api/v1".into()),
            },
            None,
        );
    }

    // Try Vertex AI via gcloud ADC (no API key, uses Google Cloud auth)
    if let Some(project_id) = detect_gcloud_project() {
        let model = args
            .model
            .clone()
            .unwrap_or_else(|| "gemini-3.1-pro-preview".into());
        let location = std::env::var("VERTEX_LOCATION")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "global".into());
        return (
            backend::SgrProvider::Vertex {
                project_id,
                model,
                location,
            },
            None,
        );
    }

    // Last resort: Gemini CLI direct subprocess
    start_sgr_gemini_cli(args).await
}

/// Detect GCP project from env or gcloud config.
fn detect_gcloud_project() -> Option<String> {
    // Explicit env var first
    if let Ok(p) = std::env::var("VERTEX_PROJECT") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    // Check if ADC credentials file exists
    let home = std::env::var("HOME").ok()?;
    let adc_path =
        std::path::PathBuf::from(&home).join(".config/gcloud/application_default_credentials.json");
    if !adc_path.exists() {
        return None;
    }
    // Get project from gcloud config (sync, but only at startup)
    let output = std::process::Command::new("gcloud")
        .args(["config", "get-value", "project"])
        .output()
        .ok()?;
    if output.status.success() {
        let project = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !project.is_empty() && !project.contains("unset") {
            return Some(project);
        }
    }
    None
}

async fn start_sgr_gemini_cli(
    args: &Args,
) -> (backend::SgrProvider, Option<tokio::task::JoinHandle<()>>) {
    // Verify gemini CLI exists
    let check = tokio::process::Command::new("which")
        .arg("gemini")
        .output()
        .await;
    if check.is_err() || !check.unwrap().status.success() {
        eprintln!("--gemini-cli requires `gemini` CLI installed");
        eprintln!("Install: https://github.com/google-gemini/gemini-cli");
        std::process::exit(1);
    }

    (
        backend::SgrProvider::GeminiCli {
            model: args.model.clone(),
            sandbox: true,
        },
        None,
    )
}

/// Apply resolved provider to agent.
fn apply_provider(setup: &ProviderSetup, agent: &mut Agent) {
    if let Some(ref provider) = setup.provider {
        agent.set_provider(provider.clone());
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
            println!(
                "Available: gemini, claude, codex, openai, anthropic, ollama, gemini-cli, codex-cli, claude-cli"
            );
        }
        ConfigAction::Set { provider, model } => {
            if providers::resolve_provider(&provider).is_none() {
                eprintln!("Unknown provider: {}", provider);
                eprintln!(
                    "Available: gemini, claude, codex, openai, anthropic, ollama, gemini-cli, codex-cli, claude-cli"
                );
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
                    || baml_agent::check_gcloud_adc()
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
                            || baml_agent::check_gcloud_adc()));
                if found {
                    true
                } else {
                    if provider_name == "vertex" {
                        eprintln!("\n\x1b[33mWarning:\x1b[0m No Google Cloud auth found.");
                        eprintln!("  Option 1: gcloud auth application-default login");
                        eprintln!(
                            "  Option 2: export GOOGLE_APPLICATION_CREDENTIALS=/path/to/key.json"
                        );
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
    use baml_agent::doctor;

    // Agent-specific extra checks (MCP, skills)
    let home = std::env::var("HOME").unwrap_or_default();
    let (mut results, mut pass, fail) = doctor::run_doctor(".rust-code", &[]);

    // MCP check (rc-cli specific)
    let mcp_global = std::path::Path::new(&home).join(".mcp.json");
    let mcp_local = std::path::Path::new(".mcp.json");
    if mcp_global.exists() || mcp_local.exists() {
        let config = tools::mcp::McpManager::load_configs();
        results.push(doctor::CheckResult {
            name: ".mcp.json".into(),
            status: doctor::CheckStatus::Ok,
            detail: format!("{} server(s) configured", config.mcp_servers.len()),
            fix: None,
        });
        pass += 1;
    } else {
        results.push(doctor::CheckResult {
            name: ".mcp.json".into(),
            status: doctor::CheckStatus::Warning,
            detail: "not found (optional, for MCP tools)".into(),
            fix: None,
        });
    }

    // Skills check (rc-cli specific)
    let skills = tools::collect_installed_skills();
    results.push(doctor::CheckResult {
        name: "skills".into(),
        status: doctor::CheckStatus::Ok,
        detail: format!("{} installed", skills.len()),
        fix: None,
    });

    doctor::print_doctor_report("rust-code", &results, pass, fail);

    if fix {
        doctor::fix_missing(&results);
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

fn run_task_command(action: TaskAction) -> Result<()> {
    let root = std::path::Path::new(".");

    match action {
        TaskAction::List { status } => {
            let tasks = baml_agent::load_tasks(root);
            let filtered: Vec<_> = if let Some(ref s) = status {
                let target = baml_agent::TaskStatus::parse(s);
                if target.is_none() {
                    eprintln!(
                        "Unknown status: {}. Use: todo, in_progress, blocked, done",
                        s
                    );
                    std::process::exit(1);
                }
                tasks
                    .into_iter()
                    .filter(|t| Some(t.status) == target)
                    .collect()
            } else {
                tasks
            };

            if filtered.is_empty() {
                println!("No tasks found.");
                return Ok(());
            }

            println!("Tasks ({}):\n", filtered.len());
            for t in &filtered {
                let marker = match t.status {
                    baml_agent::TaskStatus::Done => "\x1b[32m✓\x1b[0m",
                    baml_agent::TaskStatus::InProgress => "\x1b[33m▶\x1b[0m",
                    baml_agent::TaskStatus::Blocked => "\x1b[31m✗\x1b[0m",
                    baml_agent::TaskStatus::Todo => "\x1b[2m·\x1b[0m",
                };
                println!(
                    "  {} #{:03} [{}] ({}) {}",
                    marker, t.id, t.status, t.priority, t.title
                );
            }
        }
        TaskAction::Show { id } => {
            let tasks = baml_agent::load_tasks(root);
            match tasks.iter().find(|t| t.id == id) {
                Some(t) => {
                    println!("#{:03} {}", t.id, t.title);
                    println!("Status:   {}", t.status);
                    println!("Priority: {}", t.priority);
                    if !t.blocked_by.is_empty() {
                        println!("Blocked:  {:?}", t.blocked_by);
                    }
                    println!("File:     {}", t.path.display());
                    if !t.body.is_empty() {
                        println!("\n{}", t.body);
                    }
                }
                None => {
                    eprintln!("Task #{} not found", id);
                    std::process::exit(1);
                }
            }
        }
        TaskAction::Create { title, priority } => {
            let p = baml_agent::Priority::parse(&priority).unwrap_or(baml_agent::Priority::Medium);
            let task = baml_agent::create_task(root, &title, p);
            println!(
                "\x1b[32m✓\x1b[0m Created #{:03}: {} [{}]",
                task.id, task.title, task.priority
            );
            println!("  {}", task.path.display());
        }
        TaskAction::Done { id } => {
            match baml_agent::update_status(root, id, baml_agent::TaskStatus::Done) {
                Some(t) => println!("\x1b[32m✓\x1b[0m Completed #{:03}: {}", t.id, t.title),
                None => {
                    eprintln!("Task #{} not found", id);
                    std::process::exit(1);
                }
            }
        }
        TaskAction::Update { id, status } => {
            let Some(s) = baml_agent::TaskStatus::parse(&status) else {
                eprintln!(
                    "Unknown status: {}. Use: todo, in_progress, blocked, done",
                    status
                );
                std::process::exit(1);
            };
            match baml_agent::update_status(root, id, s) {
                Some(t) => println!("Updated #{:03}: {} → {}", t.id, t.title, t.status),
                None => {
                    eprintln!("Task #{} not found", id);
                    std::process::exit(1);
                }
            }
        }
    }
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
            Commands::Task { action } => run_task_command(action),
        };
    }

    // Auto-setup on first run if no config and no CLI flags
    // Runs BEFORE telemetry init so eprintln! is visible to user
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

    // Resolve provider BEFORE stderr redirect — auth errors must be visible
    let provider_setup = resolve_provider_setup(&args).await;

    // Initialize OTEL-aware structured telemetry (JSONL + span context)
    // TUI mode redirects stderr to file (prevents BAML output from corrupting ratatui)
    let is_headless = args.prompt.is_some();
    let _telemetry: Box<dyn std::any::Any> = if is_headless {
        Box::new(init_telemetry_headless())
    } else {
        Box::new(init_telemetry_tui())
    };

    if let Some(prompt) = args.prompt {
        // Single prompt headless mode — fresh session by default
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        // Set working directory if specified
        if let Some(ref cwd) = args.cwd {
            let path = std::path::PathBuf::from(cwd);
            if path.is_dir() {
                agent.set_cwd(path);
            } else {
                eprintln!("Warning: --cwd path does not exist: {}", cwd);
            }
        }
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
            loop_abort_threshold: 12,
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
        if let Some(ref provider) = provider_setup.provider {
            app.set_provider_override(provider.clone());
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
