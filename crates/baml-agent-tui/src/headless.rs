use baml_agent::{
    Session, LoopConfig, LoopEvent,
    run_loop_stream,
};
use crate::agent_task::TuiAgent;
use std::io::Write;

/// Run agent in headless mode (no TUI, stdout output).
///
/// Uses `run_loop_stream` for streaming tokens to stdout.
/// Same `TuiAgent` trait — so one impl covers both modes.
pub async fn run_headless<A>(
    agent: &A,
    session: &mut Session<A::Msg>,
    config: &LoopConfig,
) -> Result<usize, A::Error>
where
    A: TuiAgent + Send + Sync,
{
    run_loop_stream(agent, session, config, |event| {
        match event {
            LoopEvent::StepStart(n) => {
                println!("\n[Step {}] Thinking...", n);
            }
            LoopEvent::StreamToken(token) => {
                print!("{}", token);
                let _ = std::io::stdout().flush();
            }
            LoopEvent::Decision { state, plan } => {
                println!();
                println!("State: {}", state);
                for (i, step) in plan.iter().enumerate() {
                    println!("  {}. {}", i + 1, step);
                }
            }
            LoopEvent::Completed => {
                println!("\nTask completed!");
            }
            LoopEvent::ActionStart(action) => {
                println!("  > {}", A::action_label(action));
            }
            LoopEvent::ActionDone(result) => {
                let preview = if result.output.len() > 200 {
                    format!("{}...", &result.output[..200])
                } else {
                    result.output.clone()
                };
                println!("    = {}", preview.replace('\n', "\n    "));
            }
            LoopEvent::LoopWarning(n) => {
                eprintln!("  ! Warning: {} identical steps", n);
            }
            LoopEvent::LoopAbort(n) => {
                eprintln!("  ! Aborted after {} identical steps", n);
            }
            LoopEvent::Trimmed(n) => {
                eprintln!("  (trimmed {} old messages)", n);
            }
            LoopEvent::MaxStepsReached(n) => {
                eprintln!("  Max steps ({}) reached.", n);
            }
        }
    }).await
}
