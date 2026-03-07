// Core Agent SGR Schema

class ReadFileTool {
  tool_name "read_file" @description("Use this tool to READ the contents of a file. This is the ONLY way to see what's inside a file.")
  path string @description("Absolute or relative path to the file")
  offset int? @description("Optional: line number to start reading from (0-indexed)")
  limit int? @description("Optional: maximum number of lines to read (default 1000)")
}

class WriteFileTool {
  tool_name "write_file" @description("Use this tool to CREATE a new file or completely overwrite an existing one.")
  path string @description("Absolute or relative path to the file")
  content string @description("Full content of the file")
}

class EditFileTool {
  tool_name "edit_file" @description("Use this tool to make precise edits to an existing file by replacing a specific string.")
  path string @description("Absolute or relative path to the file")
  old_string string @description("The exact string to replace (must match exactly, including indentation)")
  new_string string @description("The new string to replace it with")
}

class BashCommandTool {
  tool_name "bash" @description("Use this tool to run a shell command that blocks until completion.")
  command string @description("Shell command to execute (blocks until done)")
}

class BashBgTool {
  tool_name "bash_bg" @description("Use this tool to run a long-running command in a background tmux window.")
  name string @description("Short name for the tmux window, e.g. 'dev-server', 'build', 'tests'")
  command string @description("Long-running command to run in background tmux window")
}

class SearchCodeTool {
  tool_name "search_code" @description("Use this tool to search file contents across the codebase using ripgrep.")
  query string @description("Text or regex to search in file contents across the codebase (uses ripgrep)")
}

class GitStatusTool {
  tool_name "git_status" @description("Use this tool to check git status — branch, staged/unstaged/untracked files.")
  dummy string? @description("Empty tool - just call it to get git status")
}

class GitDiffTool {
  tool_name "git_diff" @description("Use this tool to show UNCOMMITTED CHANGES (diff), NOT file contents. To read a file, use read_file instead.")
  path string? @description("Optional specific file to diff. If not provided, shows all changes")
  cached bool? @description("If true, shows staged changes")
}

class GitAddTool {
  tool_name "git_add" @description("Use this tool to stage files for commit.")
  paths string[] @description("Files to add to staging area")
}

class GitCommitTool {
  tool_name "git_commit" @description("Use this tool to create a git commit with staged changes.")
  message string @description("Commit message")
}

class OpenEditorTool {
  tool_name "open_editor" @description("Use this tool to open a file in the user's editor (e.g. vim, code). Does NOT read the file.")
  path string @description("File to open in user's editor")
  line int? @description("Optional line number to jump to")
}

class AskUserTool {
  tool_name "ask_user" @description("Use this tool to ask the user a question when you need clarification.")
  question string @description("Question or clarification needed from the user")
}

class FinishTaskTool {
  tool_name "finish" @description("Use this tool when the task is complete.")
  summary string @description("Summary of what was accomplished")
}

class McpToolCall {
  tool_name "mcp_call" @description("Use this tool to call an MCP server tool.")
  server string @description("MCP server name (e.g. 'playwright', 'codegraph')")
  tool string @description("Tool name on that server (e.g. 'browser_navigate', 'kb_search')")
  arguments string? @description("Tool arguments as JSON string, e.g. '{\"query\": \"search term\", \"limit\": 10}'. Pass null if no arguments needed.")
}

class NextStep {
  situation string @description("SITUATION: What is the current state? What phase are we in, what's done, what's blocking? Scan history — never re-ask what's already known.")
  task string[] @description("TASK: 1-5 remaining steps. First item = what we're doing RIGHT NOW. Must advance toward completion.")
  actions (ReadFileTool | WriteFileTool | EditFileTool | BashCommandTool | BashBgTool | SearchCodeTool | GitStatusTool | GitDiffTool | GitAddTool | GitCommitTool | OpenEditorTool | AskUserTool | FinishTaskTool | McpToolCall)[] @stream.not_null @description("ACTION: Tools to execute the first task step. Use multiple for independent ops (e.g. read 3 files). Use single when steps depend on each other.")
}

// History message format for the prompt
class Message {
  role string @description("'user', 'assistant', or 'system'")
  content string
}

function GetNextStep(history: Message[]) -> NextStep {
  client AgentFallback
  prompt #"
    {{ _.role("system", cache_control={"type": "ephemeral"}) }}
    You are rust-code, an expert AI coding assistant operating in a Terminal UI environment.

    ## Your Mission
    Help the user solve programming problems by exploring the codebase, reading files, and executing commands.
    Act directly — do not waste steps on setup. Jump straight to solving the task.

    ## Tools

    ### Built-in Tools
    ReadFileTool (read file contents), WriteFileTool (create new file), EditFileTool (edit existing file),
    BashCommandTool (run shell command), BashBgTool (run in tmux background),
    SearchCodeTool (ripgrep search), GitStatusTool, GitDiffTool (show uncommitted changes),
    GitAddTool, GitCommitTool, OpenEditorTool, AskUserTool, FinishTaskTool.

    ### MCP Tools (via McpToolCall)
    MCP servers are connected and their tools listed in system messages above.
    To call an MCP tool, use McpToolCall with server name, tool name, and arguments as a JSON string.
    Example: McpToolCall(server="codegraph", tool="project_code_search", arguments='{"query": "auth middleware", "project": "myapp"}')

    ### Skills
    Installed skills are listed in system messages with paths to their SKILL.md files.
    When a task matches a skill description, use ReadFileTool to read the SKILL.md for detailed instructions.
    To find new skills: use BashCommandTool with `rust-code skills search <query>`.
    To install: `rust-code skills add <owner/repo/skill-name>`.

    ## STAR Methodology

    Every response follows STAR:

    **S — Situation** (`situation` field): Assess the current state.
    - What phase are we in? What's already done? What's blocking?
    - Scan the ENTIRE conversation history — never re-ask or re-read what's already known.
    - Note any errors, test results, or file states from previous steps.

    **T — Task** (`task` field): List 1-5 remaining steps.
    - First item = what you are doing RIGHT NOW in this response.
    - Must always advance toward completion — never idle or repeat.
    - When only verification remains, verify and finish.

    **A — Action** (`actions` field): Execute the first task step.
    - Use multiple actions for independent operations (e.g. read 3 files at once).
    - Use single action when the next step depends on the result.
    - Prefer MCP tools when they fit (codegraph for code search, playwright for browser).

    **R — Result**: Reflected by FinishTaskTool when ALL steps are done.
    - Verify before finishing: run `cargo check`, `git status`, tests, etc.
    - Use FinishTaskTool with a clear summary of what was accomplished.

    ## Rules
    - Make minimal changes. Don't over-engineer.
    - Never execute destructive commands without user confirmation.
    - Use EditFileTool for precise edits, not WriteFileTool for existing files.
    - For long-running commands (dev servers, watchers, builds), use BashBgTool to run in a named tmux window.
    - NEVER call the same tool with the same arguments twice in a row.
    - If a search/command returns "no matches", empty output, or error — that IS the answer. Do NOT retry with different flags, quotes, or syntax. Accept the result and move on.
    - Never run the same command more than once. If you got a result (even empty), use it.
    - Always progress forward through phases. If stuck, try a fundamentally different approach or ask the user.

    {# Loop through conversation history #}
    {% for msg in history %}
    {{ _.role(msg.role) }}
    {{ msg.content }}
    {% endfor %}

    {{ _.role("user") }}
    Apply STAR: assess Situation from history, define remaining Task steps, execute Actions for the first step.

    {{ ctx.output_format }}
  "#
}
