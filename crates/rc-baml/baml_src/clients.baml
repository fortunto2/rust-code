// === Manual Provider Config ===
// We explicitly define clients so Rust core can route requests based on settings.

// 1. Google Gemini 3.1 Flash Lite Preview
client<llm> Gemini31FlashLite {
  provider google-ai
  options {
    model "gemini-3.1-flash-lite-preview"
    api_key env.GEMINI_API_KEY
  }
}

// 2. Vertex AI (Gemini 3.1 Flash Lite Preview)
client<llm> VertexGemini31FlashLite {
  provider vertex-ai
  options {
    model "gemini-3.1-flash-lite-preview"
    project_id env.GOOGLE_CLOUD_PROJECT
    location "global"
  }
}

// 3. OpenRouter (BYOK) - user brings their own key
client<llm> OpenRouter {
  provider openai-generic
  options {
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
    model "anthropic/claude-3.5-sonnet"
  }
}

retry_policy Constant {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 500
  }
}
