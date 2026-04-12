//! Structured output — call LLM, get typed Rust struct back.
//!
//! No agent loop, no tools — just LLM → JSON → Rust struct.
//!
//! Run: cargo run -p sgr-agent --features oxide --example structured_output

use schemars::JsonSchema;
use serde::Deserialize;
use sgr_agent::types::Message;
use sgr_agent::{Llm, LlmConfig};

#[derive(Debug, Deserialize, JsonSchema)]
struct Recipe {
    name: String,
    cuisine: String,
    ingredients: Vec<String>,
    steps: Vec<String>,
    prep_time_minutes: u32,
}

#[tokio::main]
async fn main() {
    let llm = Llm::new(&LlmConfig::auto("gpt-4o-mini"));
    let messages = vec![Message::user("Give me a quick pasta recipe")];

    match llm.structured::<Recipe>(&messages).await {
        Ok(recipe) => {
            println!("Recipe: {}", recipe.name);
            println!("Cuisine: {}", recipe.cuisine);
            println!("Prep: {} min", recipe.prep_time_minutes);
            println!("Ingredients: {}", recipe.ingredients.join(", "));
            for (i, step) in recipe.steps.iter().enumerate() {
                println!("  {}. {}", i + 1, step);
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
