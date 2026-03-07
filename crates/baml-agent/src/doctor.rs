//! Environment health checker for BAML agents.
//!
//! Checks system tools, provider auth, and project config.
//! Extensible via `DoctorCheck` for agent-specific checks.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Result of a single doctor check.
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
}

/// Status of a check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

/// A doctor check to run.
pub struct DoctorCheck {
    pub name: &'static str,
    pub cmd: &'static str,
    pub args: &'static [&'static str],
    pub install_brew: &'static str,
    pub install_other: &'static str,
    pub required: bool,
}

/// Default tool checks common to all BAML agents.
pub fn default_tool_checks() -> Vec<DoctorCheck> {
    vec![
        DoctorCheck {
            name: "tmux",
            cmd: "tmux",
            args: &["-V"],
            install_brew: "brew install tmux",
            install_other: "apt install tmux",
            required: true,
        },
        DoctorCheck {
            name: "ripgrep (rg)",
            cmd: "rg",
            args: &["--version"],
            install_brew: "brew install ripgrep",
            install_other: "cargo install ripgrep",
            required: true,
        },
        DoctorCheck {
            name: "git",
            cmd: "git",
            args: &["--version"],
            install_brew: "brew install git",
            install_other: "apt install git",
            required: true,
        },
    ]
}

/// Optional tool checks (python, node, cargo).
pub fn optional_tool_checks() -> Vec<DoctorCheck> {
    vec![
        DoctorCheck {
            name: "python3",
            cmd: "python3",
            args: &["--version"],
            install_brew: "brew install python3",
            install_other: "apt install python3",
            required: false,
        },
        DoctorCheck {
            name: "node",
            cmd: "node",
            args: &["--version"],
            install_brew: "brew install node",
            install_other: "curl -fsSL https://fnm.vercel.app/install | bash",
            required: false,
        },
        DoctorCheck {
            name: "cargo",
            cmd: "cargo",
            args: &["--version"],
            install_brew: "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
            install_other: "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
            required: false,
        },
    ]
}

/// Run a single tool check.
pub fn run_tool_check(check: &DoctorCheck) -> CheckResult {
    let is_mac = cfg!(target_os = "macos");
    match Command::new(check.cmd).args(check.args).output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout);
            let ver_line = ver.lines().next().unwrap_or("ok").trim().to_string();
            CheckResult {
                name: check.name.to_string(),
                status: CheckStatus::Ok,
                detail: ver_line,
                fix: None,
            }
        }
        _ => {
            let install = if is_mac {
                check.install_brew
            } else {
                check.install_other
            };
            CheckResult {
                name: check.name.to_string(),
                status: if check.required {
                    CheckStatus::Error
                } else {
                    CheckStatus::Warning
                },
                detail: "missing".to_string(),
                fix: Some(install.to_string()),
            }
        }
    }
}

/// Check if gcloud Application Default Credentials exist.
pub fn check_gcloud_adc() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home)
        .join(".config/gcloud/application_default_credentials.json")
        .exists()
}

/// Check LLM provider auth status.
pub fn check_provider_auth(provider: &str) -> CheckResult {
    match provider {
        "gemini" => check_env_key("gemini", "GEMINI_API_KEY"),
        "vertex" | "vertex-ai" => {
            if std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok() {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: "Vertex AI via service account key".into(),
                    fix: None,
                }
            } else if std::env::var("VERTEX_PROJECT").is_ok() {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: "Vertex AI via VERTEX_PROJECT".into(),
                    fix: None,
                }
            } else if check_gcloud_adc() {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: "Vertex AI via gcloud ADC".into(),
                    fix: None,
                }
            } else {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Error,
                    detail: "no Google Cloud credentials".into(),
                    fix: Some("gcloud auth application-default login".into()),
                }
            }
        }
        "claude" => {
            #[cfg(feature = "providers")]
            {
                if crate::providers::load_claude_keychain_token().is_ok() {
                    return CheckResult {
                        name: "LLM auth".into(),
                        status: CheckStatus::Ok,
                        detail: "Claude Keychain token found".into(),
                        fix: None,
                    };
                }
            }
            CheckResult {
                name: "LLM auth".into(),
                status: CheckStatus::Error,
                detail: "no Claude Keychain token".into(),
                fix: Some("run `claude` to authenticate".into()),
            }
        }
        "anthropic" => check_env_key("anthropic", "ANTHROPIC_API_KEY"),
        "openai" => check_env_key("openai", "OPENAI_API_KEY"),
        "codex" | "chatgpt" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let auth_path = PathBuf::from(&home).join(".codex/auth.json");
            if auth_path.exists() {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: "Codex auth.json found".into(),
                    fix: None,
                }
            } else {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Error,
                    detail: "Codex auth not found".into(),
                    fix: Some("codex login".into()),
                }
            }
        }
        "ollama" | "local" => {
            if Command::new("ollama")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: "ollama installed (no auth needed)".into(),
                    fix: None,
                }
            } else {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Error,
                    detail: "ollama not installed".into(),
                    fix: Some("brew install ollama".into()),
                }
            }
        }
        cli if cli.ends_with("-cli") => {
            let cmd = cli.strip_suffix("-cli").unwrap_or(cli);
            if Command::new(cmd)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Ok,
                    detail: format!("{} CLI found", cmd),
                    fix: None,
                }
            } else {
                CheckResult {
                    name: "LLM auth".into(),
                    status: CheckStatus::Error,
                    detail: format!("{} CLI not found", cmd),
                    fix: Some(format!("install {} CLI", cmd)),
                }
            }
        }
        _ => CheckResult {
            name: "LLM auth".into(),
            status: CheckStatus::Warning,
            detail: format!("unknown provider '{}'", provider),
            fix: None,
        },
    }
}

fn check_env_key(_provider: &str, var: &str) -> CheckResult {
    if std::env::var(var).is_ok() {
        CheckResult {
            name: "LLM auth".into(),
            status: CheckStatus::Ok,
            detail: format!("{} set", var),
            fix: None,
        }
    } else {
        CheckResult {
            name: "LLM auth".into(),
            status: CheckStatus::Error,
            detail: format!("{} not set", var),
            fix: Some(format!("export {}=\"your-api-key\"", var)),
        }
    }
}

/// Run all default checks + provider auth. Returns (results, pass_count, fail_count).
pub fn run_doctor(
    #[allow(unused_variables)] agent_home: &str,
    extra_checks: &[DoctorCheck],
) -> (Vec<CheckResult>, usize, usize) {
    let mut results = Vec::new();
    let mut pass = 0;
    let mut fail = 0;

    // Tool checks
    let mut all_checks = default_tool_checks();
    all_checks.extend(optional_tool_checks());
    for check in &all_checks {
        let r = run_tool_check(check);
        if r.status == CheckStatus::Ok {
            pass += 1;
        } else if check.required {
            fail += 1;
        }
        results.push(r);
    }

    // Extra agent-specific checks
    for check in extra_checks {
        let r = run_tool_check(check);
        if r.status == CheckStatus::Ok {
            pass += 1;
        } else if check.required {
            fail += 1;
        }
        results.push(r);
    }

    // Provider auth
    #[cfg(feature = "providers")]
    {
        let cfg = crate::providers::load_config(agent_home);
        let provider = cfg.provider.as_deref().unwrap_or("");
        if !provider.is_empty() {
            let r = check_provider_auth(provider);
            results.push(CheckResult {
                name: "provider".into(),
                status: CheckStatus::Ok,
                detail: provider.to_string(),
                fix: None,
            });
            if r.status == CheckStatus::Ok {
                pass += 1;
            } else {
                fail += 1;
            }
            results.push(r);
        } else {
            results.push(CheckResult {
                name: "provider".into(),
                status: CheckStatus::Warning,
                detail: "not configured".into(),
                fix: Some(format!("{} setup", agent_home.trim_start_matches('.'))),
            });
        }
    }

    (results, pass, fail)
}

/// Format check result as a colored terminal line.
pub fn format_check(r: &CheckResult) -> String {
    let icon = match r.status {
        CheckStatus::Ok => "\x1b[32m✓\x1b[0m",
        CheckStatus::Warning => "\x1b[33m-\x1b[0m",
        CheckStatus::Error => "\x1b[31m✗\x1b[0m",
    };
    let fix_str = r
        .fix
        .as_ref()
        .map(|f| format!(" [fix: {}]", f))
        .unwrap_or_default();
    format!("  {} {} — {}{}", icon, r.name, r.detail, fix_str)
}

/// Print full doctor report to stdout.
pub fn print_doctor_report(agent_name: &str, results: &[CheckResult], pass: usize, fail: usize) {
    println!("{} doctor\n", agent_name);
    for r in results {
        println!("{}", format_check(r));
    }
    let total = results
        .iter()
        .filter(|r| r.status != CheckStatus::Warning)
        .count();
    println!("\n{}/{} checks passed\n", pass, total);
    if fail == 0 {
        println!("\x1b[32mAll good!\x1b[0m {} is ready.", agent_name);
    } else {
        println!(
            "Run \x1b[1m{} doctor --fix\x1b[0m to install missing dependencies.",
            agent_name
        );
    }
}

/// Auto-fix missing tools by running install commands.
pub fn fix_missing(results: &[CheckResult]) {
    let fixable: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Error && r.fix.is_some())
        .collect();
    if fixable.is_empty() {
        return;
    }
    println!("Installing missing dependencies...\n");
    for r in &fixable {
        let cmd = r.fix.as_ref().unwrap();
        println!("  → {} ...", r.name);
        let status = Command::new("sh").arg("-c").arg(cmd).status();
        match status {
            Ok(s) if s.success() => println!("    \x1b[32m✓\x1b[0m installed"),
            Ok(s) => println!(
                "    \x1b[31m✗\x1b[0m failed (exit {})",
                s.code().unwrap_or(-1)
            ),
            Err(e) => println!("    \x1b[31m✗\x1b[0m error: {}", e),
        }
    }
    println!("\nRe-run doctor to verify.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_checks_not_empty() {
        assert!(default_tool_checks().len() >= 3);
    }

    #[test]
    fn check_git_passes() {
        // git should be available in CI and locally
        let check = DoctorCheck {
            name: "git",
            cmd: "git",
            args: &["--version"],
            install_brew: "brew install git",
            install_other: "apt install git",
            required: true,
        };
        let r = run_tool_check(&check);
        assert_eq!(r.status, CheckStatus::Ok);
        assert!(r.detail.contains("git"));
    }

    #[test]
    fn check_missing_tool() {
        let check = DoctorCheck {
            name: "nonexistent_tool_xyz",
            cmd: "nonexistent_tool_xyz_12345",
            args: &["--version"],
            install_brew: "brew install xyz",
            install_other: "apt install xyz",
            required: true,
        };
        let r = run_tool_check(&check);
        assert_eq!(r.status, CheckStatus::Error);
        assert!(r.fix.is_some());
    }

    #[test]
    fn format_ok_check() {
        let r = CheckResult {
            name: "test".into(),
            status: CheckStatus::Ok,
            detail: "1.0".into(),
            fix: None,
        };
        let s = format_check(&r);
        assert!(s.contains("test"));
        assert!(s.contains("1.0"));
    }

    #[test]
    fn provider_unknown_is_warning() {
        let r = check_provider_auth("unknown_provider_xyz");
        assert_eq!(r.status, CheckStatus::Warning);
    }

    #[test]
    fn gcloud_adc_check_runs() {
        // Just verify it doesn't panic
        let _ = check_gcloud_adc();
    }
}
