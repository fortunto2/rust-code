// Core Agent SGR Schema

class ReadFileTool {
  path string @description("Absolute or relative path to the file")
}

class WriteFileTool {
  path string @description("Absolute or relative path to the file")
  content string @description("Full content of the file")
}

class BashCommandTool {
  command string @description("Shell command to execute")
}

class SearchCodeTool {
  query string @description("Text or regex to search in the codebase")
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

class NextStep {
  analysis string @description("Analysis of the current state and task")
  plan_updates string[] @description("Brief bullet points of what needs to be done next")
  action ReadFileTool | WriteFileTool | BashCommandTool | SearchCodeTool | OpenEditorTool | AskUserTool | FinishTaskTool @description("The specific tool action to execute right now")
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
    You are rust-code, an expert AI coding assistant.
    You operate within a Terminal UI environment.
    Your task is to help the user solve their programming problems by exploring the codebase, reading files, and executing commands.
    Follow the conventions of the project you are working on.
    Always use the provided tools to verify your assumptions before making changes.

    {# Loop through conversation history #}
    {% for msg in history %}
    {{ _.role(msg.role) }}
    {{ msg.content }}
    {% endfor %}

    {{ _.role("user") }}
    Determine the next action to take based on the history above. Return your analysis, plan updates, and the specific tool call.
    {{ ctx.output_format }}
  "#
}
