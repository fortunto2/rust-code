//! Dynamic context injection for skills.
//!
//! Replaces `!command args` placeholders in skill body with real data from FileBackend.
//! Inspired by Claude Code skills pattern: `!gh pr diff` → actual diff output.
//!
//! Supported commands:
//! - `!tree [path]` — directory tree (default: /)
//! - `!read <path>` — file contents  
//! - `!list <path>` — directory listing
//! - `!search <pattern> [path]` — search for pattern
//! - `!context` — workspace date/time

use crate::backend::FileBackend;

/// Inject dynamic context into skill body.
///
/// Replaces backtick-wrapped `!command` placeholders with backend output.
/// Example: `` `!tree /` `` → actual tree output.
///
/// Max 10 injections per call (prevents abuse).
pub async fn inject<B: FileBackend>(body: &str, backend: &B) -> String {
    let re = regex::Regex::new(r"`!(\w+)\s*(.*?)`").unwrap();
    let mut result = body.to_string();
    let mut count = 0;

    // Collect matches first (can't async in replace_all)
    let matches: Vec<(String, String, String)> = re
        .captures_iter(body)
        .take(10)
        .map(|cap| {
            (
                cap[0].to_string(),
                cap[1].to_string(),
                cap[2].trim().to_string(),
            )
        })
        .collect();

    for (full_match, cmd, args) in matches {
        let output = match cmd.as_str() {
            "tree" => {
                let path = if args.is_empty() { "/" } else { &args };
                backend.tree(path, 2).await
            }
            "read" => {
                if args.is_empty() {
                    Ok("(error: !read requires path)".into())
                } else {
                    backend.read(&args, false, 0, 0).await
                }
            }
            "list" => {
                let path = if args.is_empty() { "/" } else { &args };
                backend.list(path).await
            }
            "search" => {
                let parts: Vec<&str> = args.splitn(2, ' ').collect();
                let pattern = parts.first().unwrap_or(&"");
                let root = parts.get(1).unwrap_or(&"/");
                backend.search(root, pattern, 10).await
            }
            "context" => backend.context().await,
            _ => Ok(format!("(unknown command: !{cmd})")),
        };

        let replacement = match output {
            Ok(text) => {
                // Truncate large outputs
                if text.len() > 2000 {
                    format!("{}...(truncated)", &text[..2000])
                } else {
                    text
                }
            }
            Err(e) => format!("(error: {e})"),
        };

        result = result.replacen(&full_match, &replacement, 1);
        count += 1;
        if count >= 10 {
            break;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test with a mock backend would go here
    // For now, test regex parsing

    #[test]
    fn parse_commands() {
        let re = regex::Regex::new(r"`!(\w+)\s*(.*?)`").unwrap();
        let body = "Tree: `!tree /` and file: `!read AGENTS.MD` and `!context`";
        let matches: Vec<(String, String)> = re
            .captures_iter(body)
            .map(|c| (c[1].to_string(), c[2].trim().to_string()))
            .collect();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0], ("tree".into(), "/".into()));
        assert_eq!(matches[1], ("read".into(), "AGENTS.MD".into()));
        assert_eq!(matches[2], ("context".into(), "".into()));
    }
}
