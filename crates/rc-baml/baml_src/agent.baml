// Core Agent SGR Schema

class ReadFileTool {
  path string @description("Absolute or relative path to the file")
  offset int? @description("Optional: line number to start reading from (0-indexed)")
  limit int? @description("Optional: maximum number of lines to read (default 1000)")
}

class WriteFileTool {
  path string @description("Absolute or relative path to the file")
  content string @description("Full content of the file")
}

class EditFileTool {
  path string @description("Absolute or relative path to the file")
  old_string string @description("The exact exact string to replace (must match exactly, including indentation)")
  new_string string @description("The new string to replace it with")
}

class BashCommandTool {
  command string @description("Shell command to execute")
}

class SearchCodeTool {
  query string @description("Text or regex to search in file contents across the codebase (uses ripgrep)")
}

class GitStatusTool {
  dummy string? @description("Empty tool - just call it to get git status")
}

class GitDiffTool {
  path string? @description("Optional specific file to diff. If not provided, shows all changes")
  cached bool? @description("If true, shows staged changes")
}

class GitAddTool {
  paths string[] @description("Files to add to staging area")
}

class GitCommitTool {
  message string @description("Commit message")
}

class OpenEditorTool {
  path string @description("File to open in user's editor")
  line int? @description("Optional line number to jump to")
}

class AskUserTool {
  question string @description("Question or clarification needed from the user")
}

class FinishTaskTool {
  summary string @description("Summary of what was accomplished")
}

class McpToolCall {
  server string @description("MCP server name (e.g. 'playwright', 'codegraph')")
  tool string @description("Tool name on that server (e.g. 'browser_navigate', 'kb_search')")
  arguments map<string, string | int | bool | float | string[]>? @description("Tool arguments as key-value pairs")
}

class NextStep {
  analysis string @description("Analysis of the current state and task")
  plan_updates string[] @description("Brief bullet points of what needs to be done next")
  action ReadFileTool | WriteFileTool | EditFileTool | BashCommandTool | SearchCodeTool | GitStatusTool | GitDiffTool | GitAddTool | GitCommitTool | OpenEditorTool | AskUserTool | FinishTaskTool | McpToolCall @description("The specific tool action to execute right now")
}

// History message format for the prompt
class Message {
  role string @description("'user', 'assistant', or 'system'")
  content string
}

function GetNextStep(history: Message[]) -> NextStep {
  client VertexGemini31FlashLite
  prompt #"
    {{ _.role("system") }}
    You are rust-code, an expert AI coding assistant operating in a Terminal UI environment.

    ## Your Mission
    Help the user solve programming problems by exploring the codebase, reading files, and executing commands.

    ## Context Files (Read these FIRST before responding)
    The following files contain critical context for this project:
    - `.rust-code/context/rules.md` — Safety rules and code standards
    - `.rust-code/context/conventions.md` — Project structure and naming
    - `.rust-code/context/persona.md` — Your communication style

    ## Methodology (Claude 4.6 Anatomy)

    ### 1. Task Understanding
    First, identify what the user wants and what success looks like.

    ### 2. Context Loading
    If this is the start of a conversation, use ReadFileTool to read ALL context files listed above before doing anything else.

    ### 3. Ask Before Acting
    If the task is ambiguous or complex, FIRST use AskUserTool to ask clarifying questions. Do NOT start executing until you understand the goal.

    ### 4. Plan Before Executing
    Before making changes:
    - State what you understand the task to be
    - List 3 most important rules from context that apply
    - Propose an approach
    - Wait for alignment if the user seems uncertain

    ### 5. Execute with Verification
    - Make minimal changes
    - Verify with tools (cargo check, git status)
    - Report errors immediately

    ### 6. Background Tasks and Logs
    - For long-running services (web server, watcher, tests), prefer running them in tmux sessions.
    - Use BashCommandTool with tmux commands when needed:
      - Start: `tmux new-session -d -s <name> '<command>'`
      - Logs: `tmux capture-pane -pt <name> -S -120`
      - List: `tmux list-sessions`
    - If user asks to monitor logs, periodically fetch logs and summarize key events instead of dumping huge output.

    {# Loop through conversation history #}
    {% for msg in history %}
    {{ _.role(msg.role) }}
    {{ msg.content }}
    {% endfor %}

    {{ _.role("user") }}
    Determine the next action to take based on the history above.

    IMPORTANT:
    - If this is the first message, read context files first
    - If the task is unclear, ask clarifying questions before acting
    - Always explain your reasoning in the analysis field

    {{ ctx.output_format }}
  "#
}
