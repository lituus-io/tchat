//! Reference Rust agent harness using `tchat::agent::client::Client`.
//!
//! Demonstrates the full flow an LLM-driven harness would run against a
//! local `tchat serve` daemon: post a question, retrieve similar prior
//! threads as context, generate an answer (here a deterministic stub —
//! a real harness would call its LLM), and reply inside the question's
//! thread.
//!
//! Prerequisite: `tchat serve` running in another shell.
//!
//! Run:
//!   cargo run --example agent_harness -- --space AAQAJuwMi-4 \
//!       "How do I configure HTTP_PROXY?"

use tchat::agent::client::{Client, ClientError};
use tchat::agent::events::EventFilter;
use tchat::agent::json::{EventKind, ThreadJson};

fn main() -> Result<(), ClientError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (space, question) = match parse_args(&args) {
        Some(t) => t,
        None => {
            eprintln!("usage: agent_harness --space SPACE_ID \"question text\" [--watch]");
            std::process::exit(2);
        }
    };
    let watch = args.iter().any(|a| a == "--watch");

    let client = Client::default();

    // 1. Sanity check the daemon is up.
    let health = client.health()?;
    eprintln!(
        "[harness] daemon ok (status={}, uptime={}s)",
        health.status, health.uptime_secs
    );

    // 2. Post the question. In a threaded space this creates a new
    //    topic; we need its `topic_id` to reply later.
    let posted = client.ask(&space, &question, None)?;
    eprintln!("[harness] posted question → topic_id={}", posted.topic_id);

    // 3. Pull RAG context: top 2 similar prior threads in this space.
    let results = client.search(&space, &question, 2)?;
    eprintln!(
        "[harness] retrieved {} thread(s) of prior context",
        results.threads.len()
    );
    let context = build_rag_context(&results.threads);

    // 4. Compose the answer. A real harness would call its LLM with
    //    the question + context as prompt; this stub just shows the
    //    plumbing.
    let answer = compose_stub_answer(&question, &context);
    eprintln!("[harness] composed {}-byte answer", answer.len());

    // 5. Reply inside the question's thread.
    let reply = client.reply(&space, &posted.topic_id, &answer, None)?;
    eprintln!("[harness] replied → message_id={}", reply.message_id);

    // 6. Optionally watch the thread for human follow-ups via SSE.
    if watch {
        eprintln!("\n[harness] watching SSE for new posts in this thread...");
        let filter = EventFilter {
            space_id: Some(space.clone()),
            topic_id: Some(posted.topic_id.clone()),
            kinds: vec![EventKind::MessagePosted, EventKind::MessageEdited],
        };
        let stream = client.events(filter)?;
        for event in stream.take(10) {
            match event {
                Ok(env) => {
                    let txt = env.text.unwrap_or_default();
                    let preview = if txt.len() > 80 { &txt[..80] } else { &txt };
                    eprintln!("    {:?} → {preview}", env.kind);
                }
                Err(e) => {
                    eprintln!("    stream error: {e}");
                    break;
                }
            }
        }
    }

    Ok(())
}

fn parse_args(args: &[String]) -> Option<(String, String)> {
    let mut space: Option<String> = None;
    let mut question: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--space" if i + 1 < args.len() => {
                space = Some(args[i + 1].clone());
                i += 2;
            }
            "--watch" => i += 1,
            other => {
                question.push(other.to_owned());
                i += 1;
            }
        }
    }
    let s = space?;
    let q = question.join(" ");
    if q.is_empty() {
        return None;
    }
    Some((s, q))
}

/// Format prior-thread messages as a single context blob suitable for
/// inclusion in an LLM prompt.
fn build_rag_context(threads: &[ThreadJson]) -> String {
    let mut out = String::new();
    for (i, t) in threads.iter().enumerate() {
        out.push_str(&format!("--- thread {} (id={}) ---\n", i + 1, t.topic_id));
        for m in &t.messages {
            out.push_str(&m.text);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Stub "LLM": echoes the question + a summary of the context. Replace
/// with a real LLM call (whichever HTTP client / SDK the harness uses)
/// in a production deployment.
fn compose_stub_answer(question: &str, context: &str) -> String {
    if context.trim().is_empty() {
        format!(
            "(stub answer) No prior threads matched. \
             A real harness would now ask its LLM:\n  \
             question: {question}"
        )
    } else {
        format!(
            "(stub answer) Found {} bytes of prior context. \
             A real harness would now ask its LLM:\n  \
             question: {question}\n  \
             context: {} chars",
            context.len(),
            context.len(),
        )
    }
}
