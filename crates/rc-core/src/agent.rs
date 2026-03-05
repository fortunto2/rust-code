use anyhow::Result;
use rc_baml::baml_client::{self, types};
use rc_tools::{read_file, write_file, run_command, FuzzySearcher};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub enum AgentEvent {
    Message(String),
    OpenEditor(String, Option<i64>),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMessage {
    role: String,
    content: String,
}

pub struct Agent {
    history: Vec<types::Message>,
}

impl Agent {
    pub fn new() -> Self {
        let mut agent = Self {
            history: Vec::new(),
        };
        // Attempt to load existing history
        let _ = agent.load_history();
        agent
    }

    pub fn history(&self) -> &[types::Message] {
        &self.history
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        let msg = types::Message {
            role: "user".to_string(),
            content: content.into(),
        };
        self.history.push(msg.clone());
        let _ = self.append_to_history_file(&msg);
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        let msg = types::Message {
            role: "assistant".to_string(),
            content: content.into(),
        };
        self.history.push(msg.clone());
        let _ = self.append_to_history_file(&msg);
    }

    fn append_to_history_file(&self, msg: &types::Message) -> Result<()> {
        let persisted = PersistedMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
        };
        let json = serde_json::to_string(&persisted)?;
        
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(".rust-code.jsonl")?;
            
        writeln!(file, "{}", json)?;
        Ok(())
    }

    fn load_history(&mut self) -> Result<()> {
        let path = Path::new(".rust-code.jsonl");
        if !path.exists() {
            return Ok(());
        }

        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            if let Ok(persisted) = serde_json::from_str::<PersistedMessage>(&line) {
                self.history.push(types::Message {
                    role: persisted.role,
                    content: persisted.content,
                });
            }
        }
        Ok(())
    }

    pub async fn step(&mut self) -> Result<types::NextStep> {
        let response = baml_client::async_client::B.GetNextStep.call(&self.history).await?;
        Ok(response)
    }

    pub async fn execute_action(&self, action: &types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool) -> Result<AgentEvent> {
        use types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(&cmd.path).await?;
                Ok(AgentEvent::Message(format!("File contents of {}:\n{}", cmd.path, content)))
            }
            WriteFileTool(cmd) => {
                write_file(&cmd.path, &cmd.content).await?;
                Ok(AgentEvent::Message(format!("Successfully wrote to {}", cmd.path)))
            }
            EditFileTool(cmd) => {
                rc_tools::fs::edit_file(&cmd.path, &cmd.old_string, &cmd.new_string).await?;
                Ok(AgentEvent::Message(format!("Successfully edited {}", cmd.path)))
            }
            BashCommandTool(cmd) => {
                let output = run_command(&cmd.command).await?;
                Ok(AgentEvent::Message(format!("Command output:\n{}", output)))
            }
            SearchCodeTool(cmd) => {
                // First, try fuzzy path matching just in case they are looking for a file
                let mut result = String::new();
                
                if let Ok(files) = FuzzySearcher::get_all_files().await {
                    let mut searcher = FuzzySearcher::new();
                    let matches = searcher.fuzzy_match_files(&cmd.query, &files);
                    if !matches.is_empty() {
                        result.push_str(&format!("File path matches for '{}':\n", cmd.query));
                        for (score, path) in matches.iter().take(5) {
                            if *score > 50 { // only show good matches
                                result.push_str(&format!("- {}\n", path));
                            }
                        }
                        result.push_str("\n");
                    }
                }
                
                // Then, do a full text search using ripgrep (rg) if available, fallback to grep
                result.push_str(&format!("Content search results for '{}':\n", cmd.query));
                
                // Escape quotes for bash
                let safe_query = cmd.query.replace("'", "'\\''");
                
                let search_cmd = format!("rg -n '{}' . || grep -rn '{}' .", safe_query, safe_query);
                match run_command(&search_cmd).await {
                    Ok(output) => {
                        if output.trim().is_empty() {
                            result.push_str("No content matches found.");
                        } else {
                            // truncate to avoid flooding context
                            let lines: Vec<&str> = output.lines().collect();
                            if lines.len() > 100 {
                                result.push_str(&lines[..100].join("\n"));
                                result.push_str(&format!("\n...[Truncated {} more lines]...", lines.len() - 100));
                            } else {
                                result.push_str(&output);
                            }
                        }
                    }
                    Err(_) => {
                        result.push_str("No content matches found or search tool failed.");
                    }
                }
                
                Ok(AgentEvent::Message(result))
            }
            OpenEditorTool(cmd) => {
                // We return a special event so the UI layer can suspend itself and open the editor
                Ok(AgentEvent::OpenEditor(cmd.path.clone(), cmd.line))
            }
            FinishTaskTool(cmd) => {
                Ok(AgentEvent::Message(format!("Task finished: {}", cmd.summary)))
            }
            AskUserTool(cmd) => {
                // In a real TUI, this would yield to the user prompt
                Ok(AgentEvent::Message(format!("Question for user: {}", cmd.question)))
            }
        }
    }
}
