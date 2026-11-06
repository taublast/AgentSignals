//! A session's questions and Claude's answers, read from its transcript: what the "Last answers"
//! panel lists, newest first, to find an answer lost far up in a long conversation.

use serde_json::Value;
use std::io::BufRead;

/// One question the user asked and the answer Claude ended that turn with.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub question: String,
    pub answer: String,
}

/// The session's questions and answers, newest first. Read on the caller's thread: a long
/// conversation is hundreds of MB, so call it off the UI thread.
pub fn read(session_id: &str) -> Result<Vec<Answer>, String> {
    let path = crate::restore::transcript(session_id).ok_or("no conversation found for this session")?;
    let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut list = parse(std::io::BufReader::new(file));
    list.reverse();
    Ok(list)
}

/// Transcript lines to questions and answers, oldest first. A question is a prompt the user typed
/// (not a tool result, a slash command, a message from another session or a summary); its answer
/// is the last text Claude wrote before the next prompt, the reply the user read.
fn parse(lines: impl BufRead) -> Vec<Answer> {
    let mut list: Vec<Answer> = Vec::new();
    for line in lines.lines().map_while(Result::ok) {
        // Most lines are tool calls, results and bookkeeping: skip them before parsing.
        let user = line.contains(r#""type":"user""#);
        if !user && !line.contains(r#""type":"assistant""#) {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(&line) else { continue };
        let flag = |name: &str| record.get(name).and_then(Value::as_bool).unwrap_or(false);
        if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
            continue;
        }
        let Some(text) = record.get("message").and_then(|m| m.get("content")).and_then(text_of) else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if user {
            // Commands, their output and messages from other sessions start with a tag.
            if !text.starts_with('<') {
                list.push(Answer { question: text.to_string(), answer: String::new() });
            }
        } else if let Some(last) = list.last_mut() {
            last.answer = text.to_string();
        }
    }
    list.retain(|a| !a.answer.is_empty());
    list
}

/// A message's text: the string itself, or its text blocks joined (tool calls and results have none).
fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")).filter_map(|b| b.get("text")?.as_str()).collect();
            (!texts.is_empty()).then(|| texts.join("\n\n"))
        }
        _ => None,
    }
}

/// The entries whose question or answer holds every word of `filter`, ignoring case.
pub fn matching<'a>(list: &'a [Answer], filter: &str) -> Vec<(usize, &'a Answer)> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    list.iter()
        .enumerate()
        .filter(|(_, a)| {
            let (q, t) = (a.question.to_lowercase(), a.answer.to_lowercase());
            words.iter().all(|w| q.contains(w) || t.contains(w))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_and_their_final_answers() {
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"how do I deploy?"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Checking the scripts."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Run dev/deploy.ps1."}]}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"agent prompt"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"and the logs?"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"In C:\\logs."}]}}"#,
            r#"{"type":"user","message":{"content":"question with no answer yet"}}"#,
            "not json",
        ]
        .join("\n");
        let list = parse(lines.as_bytes());
        assert_eq!(
            list,
            [
                Answer { question: "how do I deploy?".into(), answer: "Run dev/deploy.ps1.".into() },
                Answer { question: "and the logs?".into(), answer: "In C:\\logs.".into() },
            ]
        );
        let found = matching(&list, "DEPLOY run");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 0);
        assert_eq!(matching(&list, "").len(), 2);
    }
}
