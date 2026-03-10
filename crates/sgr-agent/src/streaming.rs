//! Streaming abstraction — channel-based streaming for agent output.
//!
//! Provides `StreamingSender` and `StreamingReceiver` for streaming
//! text chunks and tool results from agent execution.

use tokio::sync::mpsc;

/// A chunk of streaming output.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Text content chunk.
    Text(String),
    /// Tool call started.
    ToolStart { name: String, id: String },
    /// Tool result received.
    ToolResult { name: String, output: String },
    /// Agent step completed.
    StepDone { step: usize },
    /// Stream finished.
    Done,
    /// Error occurred.
    Error(String),
}

/// Sender side — used by agent loop to emit chunks.
#[derive(Clone)]
pub struct StreamingSender {
    tx: mpsc::UnboundedSender<StreamChunk>,
}

impl StreamingSender {
    /// Send a text chunk.
    pub fn add_text(&self, text: impl Into<String>) {
        let _ = self.tx.send(StreamChunk::Text(text.into()));
    }

    /// Signal tool execution started.
    pub fn add_tool_start(&self, name: impl Into<String>, id: impl Into<String>) {
        let _ = self.tx.send(StreamChunk::ToolStart {
            name: name.into(),
            id: id.into(),
        });
    }

    /// Send tool result.
    pub fn add_tool_result(&self, name: impl Into<String>, output: impl Into<String>) {
        let _ = self.tx.send(StreamChunk::ToolResult {
            name: name.into(),
            output: output.into(),
        });
    }

    /// Signal step completion.
    pub fn add_step_done(&self, step: usize) {
        let _ = self.tx.send(StreamChunk::StepDone { step });
    }

    /// Signal stream is complete.
    pub fn finish(&self) {
        let _ = self.tx.send(StreamChunk::Done);
    }

    /// Signal error.
    pub fn add_error(&self, err: impl Into<String>) {
        let _ = self.tx.send(StreamChunk::Error(err.into()));
    }
}

/// Receiver side — used by UI/consumer to read chunks.
pub struct StreamingReceiver {
    rx: mpsc::UnboundedReceiver<StreamChunk>,
}

impl StreamingReceiver {
    /// Receive next chunk. Returns None when sender is dropped.
    pub async fn next(&mut self) -> Option<StreamChunk> {
        self.rx.recv().await
    }

    /// Collect all chunks until Done or sender drops.
    pub async fn collect_all(&mut self) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();
        while let Some(chunk) = self.rx.recv().await {
            let is_done = matches!(chunk, StreamChunk::Done);
            chunks.push(chunk);
            if is_done {
                break;
            }
        }
        chunks
    }
}

/// Create a streaming channel pair.
pub fn streaming_channel() -> (StreamingSender, StreamingReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    (StreamingSender { tx }, StreamingReceiver { rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channel_sends_and_receives() {
        let (tx, mut rx) = streaming_channel();
        tx.add_text("hello");
        tx.add_text("world");
        tx.finish();

        let chunks = rx.collect_all().await;
        assert_eq!(chunks.len(), 3);
        assert!(matches!(&chunks[0], StreamChunk::Text(s) if s == "hello"));
        assert!(matches!(&chunks[1], StreamChunk::Text(s) if s == "world"));
        assert!(matches!(&chunks[2], StreamChunk::Done));
    }

    #[tokio::test]
    async fn tool_events() {
        let (tx, mut rx) = streaming_channel();
        tx.add_tool_start("bash", "call_0");
        tx.add_tool_result("bash", "output here");
        tx.add_step_done(1);
        tx.finish();

        let chunks = rx.collect_all().await;
        assert_eq!(chunks.len(), 4);
        assert!(matches!(&chunks[0], StreamChunk::ToolStart { name, .. } if name == "bash"));
        assert!(matches!(&chunks[1], StreamChunk::ToolResult { output, .. } if output == "output here"));
        assert!(matches!(&chunks[2], StreamChunk::StepDone { step: 1 }));
    }

    #[tokio::test]
    async fn next_returns_none_on_drop() {
        let (tx, mut rx) = streaming_channel();
        tx.add_text("one");
        drop(tx);

        assert!(matches!(rx.next().await, Some(StreamChunk::Text(_))));
        assert!(rx.next().await.is_none());
    }

    #[tokio::test]
    async fn error_chunk() {
        let (tx, mut rx) = streaming_channel();
        tx.add_error("something failed");
        tx.finish();

        let chunks = rx.collect_all().await;
        assert!(matches!(&chunks[0], StreamChunk::Error(s) if s == "something failed"));
    }

    #[tokio::test]
    async fn sender_is_clone() {
        let (tx, mut rx) = streaming_channel();
        let tx2 = tx.clone();
        tx.add_text("from tx1");
        tx2.add_text("from tx2");
        tx.finish();

        let chunks = rx.collect_all().await;
        assert_eq!(chunks.len(), 3); // 2 texts + Done
    }
}
