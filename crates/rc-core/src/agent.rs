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

    pub async fn execute_action(&self, action: &types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool) -> Result<AgentEvent> {
        use types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(&cmd.path).await?;
                Ok(AgentEvent::Message(format!("File contents of {}:\n{}", cmd.path, content)))
            }
            WriteFileTool(cmd) => {
                write_file(&cmd.path, &cmd.content).await?;
                Ok(AgentEvent::Message(format!("Successfully wrote to {}", cmd.path)))
            }
            BashCommandTool(cmd) => {
                let output = run_command(&cmd.command).await?;
                Ok(AgentEvent::Message(format!("Command output:\n{}", output)))
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
