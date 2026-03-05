pub mod tui;
pub mod app;
pub mod preview;

use anyhow::Result;
use clap::Parser;
use rc_core::Agent;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    prompt: Option<String>,
}

fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = tracing_appender::rolling::never(".", "rust-code.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();
        
    // Also suppress BAML's default stdout logging
    unsafe {
        std::env::set_var("BAML_LOG", "off");
    }
    
    guard
}

fn setup_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal so panic message is readable
        let _ = tui::restore();
        original_hook(panic_info);
    }));
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize file logging (keeps stdout clean for TUI)
    let _log_guard = init_logging();
    
    // Setup panic hook to restore terminal
    setup_panic_hook();
    
    // Initialize BAML runtime
    rc_baml::init();

    if let Some(prompt) = args.prompt {
        // Single prompt headless mode
        println!("Running single prompt mode...");
        let mut agent = Agent::new();
        agent.add_user_message(prompt);
        
        loop {
            println!("Thinking...");
            let step = agent.step().await?;
            
            println!("\nAnalysis: {}", step.analysis);
            println!("Plan updates:");
            for p in &step.plan_updates {
                println!(" - {}", p);
            }
            
            println!("\nAction:");
            println!("{:?}", step.action);
            
            agent.add_assistant_message(format!(
                "Analysis: {}\nAction: {:?}", 
                step.analysis, step.action
            ));
            
            let is_done = matches!(
                step.action,
                rc_baml::baml_client::types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                rc_baml::baml_client::types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
            );

            let result = agent.execute_action(&step.action).await?;
            match result {
                rc_core::AgentEvent::Message(msg) => {
                    println!("\nTool Result:\n{}", msg);
                    agent.add_user_message(format!("Tool result:\n{}", msg));
                }
                rc_core::AgentEvent::OpenEditor(path, _) => {
                    println!("\nAction requested opening editor for: {}", path);
                    agent.add_user_message(format!("Tool result:\nUser opened editor for {}", path));
                }
            }
            
            if is_done {
                break;
            }
            println!("----------------------------------------");
        }
    } else {
        // Interactive TUI mode
        let mut terminal = tui::init()?;
        let mut app = app::App::new();
        
        let result = app.run(&mut terminal).await;
        
        tui::restore()?;
        
        if let Err(err) = result {
            println!("Error running TUI: {:?}", err);
        }
    }
    
    Ok(())
}
