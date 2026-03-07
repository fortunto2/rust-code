// === LLM Client Config ===

// Primary: Gemini 3.1 Pro Preview (best structured output quality)
client<llm> Gemini31Pro {
  provider google-ai
  retry_policy Backoff
  options {
    model "gemini-3.1-pro-preview"
    api_key env.GEMINI_API_KEY
  }
}

// Primary via Vertex AI (Google Cloud project auth)
client<llm> VertexGemini {
  provider vertex-ai
  retry_policy Backoff
  options {
    model "gemini-3.1-pro-preview"
    project_id env.VERTEX_PROJECT
    location "global"
  }
}

// Fast: Gemini 2.5 Flash (native google-ai)
client<llm> Gemini25Flash {
  provider google-ai
  retry_policy Backoff
  options {
    model "gemini-2.5-flash"
    api_key env.GEMINI_API_KEY
  }
}

// Budget: Gemini 3.1 Flash Lite (fast, unreliable tool selection with large unions)
client<llm> Gemini31FlashLite {
  provider google-ai
  retry_policy Backoff
  options {
    model "gemini-3.1-flash-lite-preview"
    api_key env.GEMINI_API_KEY
  }
}

// Fallback chain: Pro → Flash → Flash Lite
client<llm> AgentFallback {
  provider fallback
  options {
    strategy [
      Gemini31Pro
      Gemini25Flash
      Gemini31FlashLite
    ]
  }
}

// Round-robin for load balancing (Pro + Flash)
client<llm> AgentBalanced {
  provider round-robin
  options {
    strategy [
      Gemini31Pro
      Gemini25Flash
    ]
  }
}

// Anthropic Claude with prompt caching
client<llm> Claude {
  provider anthropic
  retry_policy Backoff
  options {
    model "claude-sonnet-4-6"
    api_key env.ANTHROPIC_API_KEY
    max_tokens 8192
    allowed_role_metadata ["cache_control"]
    headers {
      "anthropic-beta" "prompt-caching-2024-07-31"
    }
  }
}

// OpenAI GPT-4.1 (or Codex)
client<llm> OpenAI {
  provider openai
  retry_policy Backoff
  options {
    model "gpt-4.1"
    api_key env.OPENAI_API_KEY
  }
}

// Codex Proxy: ChatGPT Plus/Pro subscription via local proxy
// Proxy converts Chat Completions → Codex Responses API (chatgpt.com)
client<llm> CodexProxy {
  provider openai
  retry_policy Backoff
  options {
    model "gpt-5.3-codex"
    base_url env.CODEX_PROXY_URL
    api_key "codex-proxy"
  }
}

// OpenAI o3-mini (reasoning, cheaper)
client<llm> OpenAIO3 {
  provider openai
  retry_policy Backoff
  options {
    model "o3-mini"
    api_key env.OPENAI_API_KEY
  }
}

// CLI Provider: any local CLI tool (claude, gemini, codex) via proxy
client<llm> CliProxy {
  provider openai
  retry_policy Backoff
  options {
    model "cli"
    base_url env.CLI_PROXY_URL
    api_key "cli-proxy"
  }
}

// === Local Models (Ollama) ===

// Local: any Ollama model (default: qwen2.5-coder)
// Start with: ollama serve && ollama pull qwen2.5-coder:14b
client<llm> OllamaDefault {
  provider ollama
  options {
    model "qwen2.5-coder:14b"
  }
}

// Local: smaller model for fast tasks
client<llm> OllamaSmall {
  provider ollama
  options {
    model "qwen2.5-coder:7b"
  }
}

// Local-first fallback: Ollama → Cloud
client<llm> LocalFirst {
  provider fallback
  options {
    strategy [
      OllamaDefault
      Gemini31Pro
    ]
  }
}

retry_policy Backoff {
  max_retries 3
  strategy {
    type exponential_backoff
    delay_ms 500
    multiplier 2
    max_delay_ms 10000
  }
}
