//! Background tasks: Claude starts a background Bash command; the SDK reports
//! snapshots of the task tree (AgentEvent::BackgroundTasks → TaskTree).
//!
//! Run: cargo run --example background
use claude_code_agent::background::TaskTree;
use claude_code_agent::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await?;
    let agent = Agent::new(session.clone());
    let mut tree = TaskTree::new();

    let mut stream = agent
        .prompt("Run `sleep 2 && echo done` with the Bash tool using run_in_background: true, then reply only: started")
        .await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::BackgroundTasks(tasks) => {
                tree.update(tasks);
                for t in tree.tasks() {
                    println!("task {} [{}] {}", t.task_id, t.task_type, t.description);
                }
            }
            AgentEvent::Text(t) => println!("claude: {t}"),
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    println!("{} task(s) running when the turn ended", tree.tasks().len());
    session.close().await;
    Ok(())
}
