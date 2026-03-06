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
  command string @description("Shell command to execute (blocks until done)")
}

class BashBgTool {
  name string @description("Short name for the tmux window, e.g. 'dev-server', 'build', 'tests'")
  command string @description("Long-running command to run in background tmux window")
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
  arguments string? @description("Tool arguments as JSON string, e.g. '{\"query\": \"search term\", \"limit\": 10}'. Pass null if no arguments needed.")
}

class NextStep {
  analysis string @description("Analysis of the current state and task")
  plan_updates string[] @description("Brief bullet points of what needs to be done next")
  action ReadFileTool | WriteFileTool | EditFileTool | BashCommandTool | BashBgTool | SearchCodeTool | GitStatusTool | GitDiffTool | GitAddTool | GitCommitTool | OpenEditorTool | AskUserTool | FinishTaskTool | McpToolCall @description("The specific tool action to execute right now")
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
    Act directly — do not waste steps on setup. Jump straight to solving the task.

    ## Tools

    ### Built-in Tools
    ReadFileTool, WriteFileTool, EditFileTool, BashCommandTool, BashBgTool,
    SearchCodeTool, GitStatusTool, GitDiffTool, GitAddTool, GitCommitTool,
    OpenEditorTool, AskUserTool, FinishTaskTool.

    ### MCP Tools (via McpToolCall)
    MCP servers are connected and their tools listed in system messages above.
    To call an MCP tool, use McpToolCall with server name, tool name, and arguments as a JSON string.
    Example: McpToolCall(server="codegraph", tool="project_code_search", arguments='{"query": "auth middleware", "project": "myapp"}')

    ### Skills
    Installed skills are listed in system messages with paths to their SKILL.md files.
    When a task matches a skill description, use ReadFileTool to read the SKILL.md for detailed instructions.
    To find new skills: use BashCommandTool with `rust-code skills search <query>`.
    To install: `rust-code skills add <owner/repo/skill-name>`.

    ## Methodology

    1. **Understand** — Identify what the user wants. If unclear, use AskUserTool.
    2. **Plan** — State approach in plan_updates. Keep it brief.
    3. **Execute** — Use the right tool. Prefer MCP tools when they fit (e.g. codegraph for code search, playwright for browser).
    4. **Verify** — Run `cargo check`, `git status`, or other verification after changes.
    5. **Report** — Use FinishTaskTool with a clear summary.

    ## Rules
    - Make minimal changes. Don't over-engineer.
    - Never execute destructive commands without user confirmation.
    - Use EditFileTool for precise edits, not WriteFileTool for existing files.
    - For long-running commands (dev servers, watchers, builds), use BashBgTool to run in a named tmux window. User can see output via F7 > Ctrl+O.
    - Always explain reasoning in the analysis field.

    {# Loop through conversation history #}
    {% for msg in history %}
    {{ _.role(msg.role) }}
    {{ msg.content }}
    {% endfor %}

    {{ _.role("user") }}
    Determine the next action to take based on the history above.

    {{ ctx.output_format }}
  "#
}
