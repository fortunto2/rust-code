// === Manual Provider Config ===
// We explicitly define clients so Rust core can route requests based on settings.

// 1. Gemini 2.5 Flash via OpenRouter (reliable structured JSON output)
client<llm> Gemini25Flash {
  provider openai-generic
  options {
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
    model "google/gemini-2.5-flash"
  }
}

// 2. Google Gemini 3.1 Flash Lite (native google-ai, fast but unreliable tool selection)
client<llm> Gemini31FlashLite {
  provider google-ai
  options {
    model "gemini-3.1-flash-lite-preview"
    api_key env.GEMINI_API_KEY
  }
}

// 3. Gemini 2.5 Flash via native Google AI (for non-structured use)
client<llm> Gemini25FlashNative {
  provider google-ai
  options {
    model "gemini-2.5-flash"
    api_key env.GEMINI_API_KEY
  }
}

// 4. Claude 3.5 Sonnet via OpenRouter
client<llm> Claude35Sonnet {
  provider openai-generic
  options {
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
    model "anthropic/claude-sonnet-4"
  }
}

// 5. Vertex AI (Gemini 2.5 Flash)
client<llm> VertexGemini25Flash {
  provider vertex-ai
  options {
    model "gemini-2.5-flash"
    project_id env.GOOGLE_CLOUD_PROJECT
    location "global"
  }
}

retry_policy Constant {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 500
  }
}
