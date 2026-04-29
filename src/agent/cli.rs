//! CLI subcommand handlers for the agent surface.
//!
//! Each subcommand probes `127.0.0.1:7800/v1/health`. If the daemon is
//! up, the CLI proxies the call via `ureq` and prints the JSON. If not,
//! the CLI builds a one-shot `AgentApi` (which authenticates and runs
//! the proto call), prints the result, and exits.
//!
//! Output is JSON when `--json` is passed, plain text otherwise.

use std::io::Read;
use std::sync::Arc;

use crate::agent::json::{
    AskRequest, AskResponse, HealthResponse, ReplyRequest, ReplyResponse, SearchResponse,
    SpacesResponse,
};
use crate::agent::server::ServerState;
use crate::agent::AgentApi;
use crate::error::AppError;

const DEFAULT_BIND: &str = "127.0.0.1:7800";

#[derive(Debug, Default)]
struct ParsedArgs {
    flags: std::collections::HashMap<String, String>,
    positional: Vec<String>,
}

fn parse_flags(args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(rest) = a.strip_prefix("--") {
            // --flag=value or --flag value
            if let Some((k, v)) = rest.split_once('=') {
                out.flags.insert(k.to_owned(), v.to_owned());
                i += 1;
            } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                out.flags.insert(rest.to_owned(), args[i + 1].clone());
                i += 2;
            } else {
                // boolean flag
                out.flags.insert(rest.to_owned(), String::new());
                i += 1;
            }
        } else {
            out.positional.push(a.clone());
            i += 1;
        }
    }
    out
}

/// Entry point. Returns `Ok(true)` if a subcommand handled the args,
/// `Ok(false)` if no recognized subcommand was found (caller should fall
/// through to the TUI), or `Err` on real failures.
pub fn dispatch(args: &[String]) -> Result<bool, AppError> {
    let cmd = match args.first() {
        Some(c) => c.as_str(),
        None => return Ok(false),
    };
    let rest = &args[1..];

    match cmd {
        "serve" => {
            run_serve(rest)?;
            Ok(true)
        }
        "ask" => {
            run_ask(rest)?;
            Ok(true)
        }
        "search" => {
            run_search(rest)?;
            Ok(true)
        }
        "reply" => {
            run_reply(rest)?;
            Ok(true)
        }
        "spaces" => {
            run_spaces(rest)?;
            Ok(true)
        }
        "health" => {
            run_health(rest)?;
            Ok(true)
        }
        "--help" | "-h" | "help" => {
            print_help();
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn print_help() {
    eprintln!(
        r#"tchat — terminal multi-platform chat client

Usage:
  tchat                                          run the TUI
  tchat serve [--port 7800] [--bind 127.0.0.1]   run agent HTTP daemon
  tchat ask --space ID "question text"           post a question (top-level)
  tchat search --space ID [--top 2] "query"      search threads in a space
  tchat reply --space ID --topic ID "answer"     reply inside a thread
  tchat spaces [list]                            list the user's spaces
  tchat health                                   ping the running daemon
  tchat help                                     show this message

Subcommands probe http://127.0.0.1:7800 first; if no daemon is running
they fall back to a one-shot in-process call.
"#
    );
}

// ───────── serve ─────────

fn run_serve(args: &[String]) -> Result<(), AppError> {
    let parsed = parse_flags(args);
    let port = parsed
        .flags
        .get("port")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(7800);
    let bind = parsed
        .flags
        .get("bind")
        .map(String::as_str)
        .unwrap_or("127.0.0.1");
    let addr = format!("{bind}:{port}");

    eprintln!("[serve] bootstrapping AgentApi (auth chain may pop Chrome)...");
    let api = AgentApi::bootstrap()?;
    eprintln!("[serve] ✓ ready");
    let state = Arc::new(ServerState::new(api));
    crate::agent::server::run(&addr, state)
}

// ───────── ask ─────────

fn run_ask(args: &[String]) -> Result<(), AppError> {
    let parsed = parse_flags(args);
    let space = require_flag(&parsed, "space")?;
    let text = parsed.positional.join(" ");
    if text.is_empty() {
        return Err(cli_err("missing question text (positional argument)"));
    }
    let json = parsed.flags.contains_key("json");
    let idem = parsed.flags.get("idempotency-key").cloned();

    let body = AskRequest {
        text,
        idempotency_key: idem,
    };
    let resp: AskResponse = call_or_run(
        &format!("/v1/spaces/{space}/questions"),
        "POST",
        Some(serde_json::to_string(&body).unwrap()),
        |api| api.post_question(&space, &body.text, body.idempotency_key.as_deref()),
    )?;
    if json {
        println!("{}", serde_json::to_string(&resp).unwrap());
    } else {
        println!("{}", resp.topic_id);
    }
    Ok(())
}

// ───────── search ─────────

fn run_search(args: &[String]) -> Result<(), AppError> {
    let parsed = parse_flags(args);
    let space = require_flag(&parsed, "space")?;
    let query = parsed.positional.join(" ");
    if query.is_empty() {
        return Err(cli_err("missing query text (positional argument)"));
    }
    let top: usize = parsed
        .flags
        .get("top")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    let endpoint = format!(
        "/v1/spaces/{space}/threads/search?q={}&top={top}",
        url_encode(&query)
    );
    let resp: SearchResponse = call_or_run::<SearchResponse, _>(&endpoint, "GET", None, |api| {
        api.search_threads(&space, &query, top)
    })?;
    println!("{}", serde_json::to_string(&resp).unwrap());
    Ok(())
}

// ───────── reply ─────────

fn run_reply(args: &[String]) -> Result<(), AppError> {
    let parsed = parse_flags(args);
    let space = require_flag(&parsed, "space")?;
    let topic = require_flag(&parsed, "topic")?;
    let text = parsed.positional.join(" ");
    if text.is_empty() {
        return Err(cli_err("missing reply text (positional argument)"));
    }
    let json_out = parsed.flags.contains_key("json");
    let idem = parsed.flags.get("idempotency-key").cloned();

    let body = ReplyRequest {
        space_id: space.clone(),
        text,
        idempotency_key: idem,
    };
    let resp: ReplyResponse = call_or_run::<ReplyResponse, _>(
        &format!("/v1/threads/{topic}/reply"),
        "POST",
        Some(serde_json::to_string(&body).unwrap()),
        |api| api.reply_in_thread(&space, &topic, &body.text, body.idempotency_key.as_deref()),
    )?;
    if json_out {
        println!("{}", serde_json::to_string(&resp).unwrap());
    } else {
        println!("{}", resp.message_id);
    }
    Ok(())
}

// ───────── spaces ─────────

fn run_spaces(_args: &[String]) -> Result<(), AppError> {
    let resp: SpacesResponse =
        call_or_run::<SpacesResponse, _>("/v1/spaces", "GET", None, |api| {
            api.list_spaces().map(|spaces| SpacesResponse { spaces })
        })?;
    println!("{}", serde_json::to_string(&resp).unwrap());
    Ok(())
}

// ───────── health ─────────

fn run_health(_args: &[String]) -> Result<(), AppError> {
    match probe_daemon() {
        Some(resp) => {
            println!("{}", serde_json::to_string(&resp).unwrap());
            Ok(())
        }
        None => {
            eprintln!("daemon not running on http://{DEFAULT_BIND}");
            std::process::exit(2);
        }
    }
}

// ───────── routing helpers ─────────

/// Probe the daemon. If it's up, do an HTTP request via `ureq`. Else,
/// build a one-shot `AgentApi` and run the closure.
fn call_or_run<R, F>(
    endpoint: &str,
    method: &str,
    body: Option<String>,
    one_shot: F,
) -> Result<R, AppError>
where
    R: for<'de> serde::Deserialize<'de> + serde::Serialize,
    F: FnOnce(&mut AgentApi) -> Result<R, AppError>,
{
    if probe_daemon().is_some() {
        return http_call(endpoint, method, body);
    }
    eprintln!("[cli] no daemon at {DEFAULT_BIND} — running one-shot...");
    let mut api = AgentApi::bootstrap()?;
    one_shot(&mut api)
}

fn http_call<R: for<'de> serde::Deserialize<'de>>(
    endpoint: &str,
    method: &str,
    body: Option<String>,
) -> Result<R, AppError> {
    let url = format!("http://{DEFAULT_BIND}{endpoint}");
    let resp = match method {
        "GET" => ureq::get(&url).call(),
        "POST" => {
            let req = ureq::post(&url).header("Content-Type", "application/json");
            req.send(body.unwrap_or_default().as_bytes())
        }
        _ => return Err(cli_err(&format!("unsupported method: {method}"))),
    };
    let resp = resp.map_err(|e| cli_err(&format!("HTTP {method} {endpoint}: {e}")))?;
    let mut text = String::new();
    resp.into_body()
        .into_reader()
        .read_to_string(&mut text)
        .map_err(|e| cli_err(&format!("read response: {e}")))?;
    serde_json::from_str(&text).map_err(|e| {
        cli_err(&format!(
            "parse JSON ({}…): {e}",
            &text[..text.len().min(200)]
        ))
    })
}

fn probe_daemon() -> Option<HealthResponse> {
    let url = format!("http://{DEFAULT_BIND}/v1/health");
    let resp = ureq::get(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_millis(500)))
        .build()
        .call()
        .ok()?;
    let mut text = String::new();
    resp.into_body()
        .into_reader()
        .read_to_string(&mut text)
        .ok()?;
    let parsed: HealthResponse = serde_json::from_str(&text).ok()?;
    Some(parsed)
}

// ───────── small helpers ─────────

fn require_flag(parsed: &ParsedArgs, name: &str) -> Result<String, AppError> {
    parsed
        .flags
        .get(name)
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| cli_err(&format!("missing required flag --{name}")))
}

fn cli_err(msg: &str) -> AppError {
    AppError::Auth(crate::error::AuthError::Http(msg.to_owned()))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => {
                out.push('%');
                out.push_str(&format!("{:02X}", other));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags_handles_value_with_separate_arg() {
        let args = vec!["--space".into(), "AAQA".into(), "hello".into()];
        let p = parse_flags(&args);
        assert_eq!(p.flags.get("space").map(String::as_str), Some("AAQA"));
        assert_eq!(p.positional, vec!["hello".to_string()]);
    }

    #[test]
    fn parse_flags_handles_equals_form() {
        let args = vec!["--top=3".into(), "query".into()];
        let p = parse_flags(&args);
        assert_eq!(p.flags.get("top").map(String::as_str), Some("3"));
        assert_eq!(p.positional, vec!["query".to_string()]);
    }

    #[test]
    fn parse_flags_handles_boolean_flag_at_end() {
        let args = vec!["--json".into()];
        let p = parse_flags(&args);
        assert!(p.flags.contains_key("json"));
        assert_eq!(p.flags.get("json").map(String::as_str), Some(""));
    }

    #[test]
    fn parse_flags_collects_positional_after_flags() {
        let args = vec![
            "--space".into(),
            "S".into(),
            "--top".into(),
            "5".into(),
            "the".into(),
            "query".into(),
            "text".into(),
        ];
        let p = parse_flags(&args);
        assert_eq!(p.positional, vec!["the", "query", "text"]);
    }

    #[test]
    fn url_encode_preserves_unreserved() {
        assert_eq!(url_encode("hello"), "hello");
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn url_encode_escapes_special() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a+b"), "a%2Bb");
    }
}
