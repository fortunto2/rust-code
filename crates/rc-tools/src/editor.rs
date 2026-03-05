use anyhow::Result;
use std::process::Command;

/// Opens the specified file in the user's default editor (falls back to vim).
/// This function blocks until the editor is closed.
pub fn open_in_editor(path: &str, line: Option<i64>) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

    let mut cmd = Command::new(&editor);

    // Most standard editors (vim, nvim, nano) support +line syntax
    if let Some(l) = line {
        // specifically for neovim/vim
        if editor.contains("vi") || editor.contains("nano") {
            cmd.arg(format!("+{}", l));
        }
    }

    cmd.arg(path);

    // We must spawn this synchronously because it takes over the terminal
    let mut child = cmd.spawn()?;
    child.wait()?;

    Ok(())
}
