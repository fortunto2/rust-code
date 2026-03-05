use anyhow::Result;
use rc_baml::baml_client::{self, types};
use rc_tools::{read_file, write_file, run_command, FuzzySearcher};

pub struct Agent {
    history: Vec<types::Message>,
}

impl Agent {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.history.push(types::Message {
            role: "user".to_string(),
            content: content.into(),
        });
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.history.push(types::Message {
            role: "assistant".to_string(),
            content: content.into(),
        });
    }

    pub async fn step(&mut self, system_prompt: &str) -> Result<types::NextStep> {
        let response = baml_client::async_client::B.GetNextStep.call(system_prompt.to_string(), &self.history).await?;
        Ok(response)
    }

    pub async fn execute_action(&self, action: &types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool) -> Result<String> {
        use types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(&cmd.path).await?;
                Ok(format!("File contents of {}:\n{}", cmd.path, content))
            }
            WriteFileTool(cmd) => {
                write_file(&cmd.path, &cmd.content).await?;
                Ok(format!("Successfully wrote to {}", cmd.path))
            }
            BashCommandTool(cmd) => {
                let output = run_command(&cmd.command).await?;
                Ok(format!("Command output:\n{}", output))
            }
            SearchCodeTool(cmd) => {
                // Implement basic file path search first
                let files = FuzzySearcher::get_all_files().await?;
                let mut searcher = FuzzySearcher::new();
                let matches = searcher.fuzzy_match_files(&cmd.query, &files);
                
                let mut result = format!("Search results for '{}':\n", cmd.query);
                for (score, path) in matches.iter().take(10) {
                    result.push_str(&format!("{} (score: {})\n", path, score));
                }
                if matches.is_empty() {
                    result.push_str("No matches found.");
                }
                Ok(result)
            }
            FinishTaskTool(cmd) => {
                Ok(format!("Task finished: {}", cmd.summary))
            }
            AskUserTool(cmd) => {
                // In a real TUI, this would yield to the user prompt
                Ok(format!("Question for user: {}", cmd.question))
            }
            _ => {
                Ok("Tool not fully implemented yet".to_string())
            }
        }
    }
}
