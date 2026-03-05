use anyhow::Result;
use clap::Parser;
use rc_core::Agent;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize BAML runtime
    rc_baml::init();

    if let Some(prompt) = args.prompt {
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        agent.add_user_message(prompt);
        
        println!("Thinking...");
        let step = agent.step("You are a helpful coding assistant. Use the tools provided.").await?;
        
        println!("\nAnalysis: {}", step.analysis);
        println!("Plan updates:");
        for p in step.plan_updates {
            println!(" - {}", p);
        }
        
        println!("\nAction:");
        println!("{:?}", step.action);
        
        let result = agent.execute_action(&step.action).await?;
        println!("\nTool Result:\n{}", result);
    } else {
        println!("TUI mode not yet fully implemented. Try --prompt \"what is in this dir?\"");
    }
    
    Ok(())
}
