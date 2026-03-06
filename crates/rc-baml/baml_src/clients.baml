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

retry_policy Backoff {
  max_retries 3
  strategy {
    type exponential_backoff
    delay_ms 500
    multiplier 2
    max_delay_ms 10000
  }
}
