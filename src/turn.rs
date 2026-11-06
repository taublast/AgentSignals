//! Whether Claude's turn is over, read from the end of the session's transcript: Claude Code's
//! own status says "busy" while a background shell or server runs, and "idle" when Claude ended its
//! turn with a question, though in both cases the session waits for the user.

use crate::state::Session;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::SystemTime;

/// The end of a transcript holds the last turn; long final answers fit in it too.
const TAIL: u64 = 512 * 1024;

/// How the last turn of a session ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    /// Claude is still at it: a tool runs, or the user just wrote.
    Going,
    /// Claude ended its turn; `question` is what it asked, when it asked something.
    Over { question: Option<String>, at_ms: u64 },
}

/// The transcripts' last turns, read again only when a transcript changed.
#[derive(Default)]
pub struct Turns {
    seen: HashMap<String, (PathBuf, SystemTime, u64, Option<Turn>)>,
}

impl Turns {
    /// Corrects the sessions' statuses from their transcripts: a turn that is over is not work, and
    /// one that ended with a question waits for the user, as a permission dialog does.
    pub fn apply(&mut self, sessions: &mut HashMap<u32, Session>) {
        let ids: Vec<&String> = sessions.values().map(|s| &s.id).collect();
        self.seen.retain(|id, _| ids.contains(&id));
        for s in sessions.values_mut().filter(|s| !s.id.is_empty() && s.status != "waiting") {
            let Some(Turn::Over { question, at_ms }) = self.turn(s) else { continue };
            match question {
                Some(q) => {
                    s.status = "waiting".into();
                    s.waiting_for = q;
                }
                None if s.status == "busy" => s.status = "idle".into(),
                None => continue,
            }
            if at_ms > 0 {
                s.since_ms = at_ms;
            }
        }
    }

    fn turn(&mut self, s: &Session) -> Option<Turn> {
        let path = match self.seen.get(&s.id) {
            Some((path, ..)) => path.clone(),
            None => transcript_path(&s.cwd, &s.id).or_else(|| crate::restore::transcript(&s.id))?,
        };
        let meta = std::fs::metadata(&path).ok()?;
        let (modified, len) = (meta.modified().ok()?, meta.len());
        if let Some((_, m, l, turn)) = self.seen.get(&s.id) {
            if *m == modified && *l == len {
                return turn.clone();
            }
        }
        let turn = read_tail(&path, len).map(|tail| last_turn(&tail));
        self.seen.insert(s.id.clone(), (path, modified, len, turn.clone()));
        turn
    }
}

/// Where Claude Code keeps a session's transcript: its folder named after the session's cwd.
fn transcript_path(cwd: &str, id: &str) -> Option<PathBuf> {
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = crate::platform::home_dir().join(".claude").join("projects").join(slug).join(format!("{id}.jsonl"));
    path.is_file().then_some(path)
}

fn read_tail(path: &PathBuf, len: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

/// The last turn in transcript lines: over when the last message of the main conversation is
/// Claude's, stopped because its turn ended (not to run a tool).
fn last_turn(tail: &str) -> Turn {
    for line in tail.lines().rev() {
        let user = line.contains(r#""type":"user""#);
        if !user && !line.contains(r#""type":"assistant""#) {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
        if record.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if user {
            return Turn::Going;
        }
        let message = record.get("message");
        let stop = message.and_then(|m| m.get("stop_reason")).and_then(Value::as_str);
        if !matches!(stop, Some("end_turn" | "stop_sequence")) {
            return Turn::Going;
        }
        // The turn's final text: this record's, or the text just before a closing thinking block.
        let text = final_text(tail, line);
        let at_ms = record.get("timestamp").and_then(Value::as_str).and_then(crate::answers::epoch_ms).unwrap_or(0);
        return Turn::Over { question: text.as_deref().and_then(question), at_ms };
    }
    Turn::Going
}

/// The text of the last assistant message at or before `line`.
fn final_text(tail: &str, line: &str) -> Option<String> {
    let end = tail.rfind(line)? + line.len();
    tail[..end].lines().rev().filter(|l| l.contains(r#""type":"assistant""#)).find_map(|l| {
        let record = serde_json::from_str::<Value>(l).ok()?;
        let blocks = record.get("message")?.get("content")?.as_array()?;
        let texts: Vec<&str> = blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")).filter_map(|b| b.get("text")?.as_str()).collect();
        (!texts.is_empty()).then(|| texts.join("\n\n"))
    })
}

/// What a final answer asks the user, if it asks something: the last line with a question mark
/// that ends a sentence, outside code; else an "Open questions" heading's first item.
// ponytail: a guess from the text; a rhetorical question turns the row yellow once.
pub fn question(text: &str) -> Option<String> {
    let mut code = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        if !code {
            lines.push(line);
        }
    }
    let clean = |line: &str| {
        let line = line.trim().trim_start_matches(['-', '*', '>', '#', ' ']).replace("**", "").replace('`', "");
        let line = line.trim();
        if line.chars().count() > 160 { format!("{}…", line.chars().take(159).collect::<String>()) } else { line.to_string() }
    };
    let asks = |line: &str| {
        let bytes: Vec<char> = line.chars().collect();
        bytes.iter().enumerate().any(|(i, c)| *c == '?' && bytes.get(i + 1).is_none_or(|n| n.is_whitespace() || matches!(n, '"' | '\'' | ')' | '*' | '`' | '»' | '”')))
    };
    if let Some(line) = lines.iter().rev().find(|l| asks(l)) {
        return Some(clean(line));
    }
    let heading = lines.iter().position(|l| l.to_lowercase().contains("open question"))?;
    lines[heading + 1..].iter().find(|l| !l.trim().is_empty()).map(|l| clean(l)).or_else(|| Some(clean(lines[heading])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_over_or_going() {
        let ask = r#"{"type":"assistant","timestamp":"2026-11-06T06:41:04.738Z","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Built and tested.\n\nShould I push it?"}]}}"#;
        let done = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Pushed, CI is green."}]}}"#;
        let tool = r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","name":"Bash"}]}}"#;
        let user = r#"{"type":"user","message":{"content":"go"}}"#;
        let side = r#"{"type":"user","isSidechain":true,"message":{"content":"agent work"}}"#;
        let meta = r#"{"type":"custom-title","customTitle":"x"}"#;
        assert_eq!(last_turn(&[user, ask, side, meta].join("\n")), Turn::Over { question: Some("Should I push it?".into()), at_ms: 1_793_947_264_738 });
        assert_eq!(last_turn(&[user, done].join("\n")), Turn::Over { question: None, at_ms: 0 });
        assert_eq!(last_turn(&[ask, user].join("\n")), Turn::Going);
        assert_eq!(last_turn(&[user, tool].join("\n")), Turn::Going);
        assert_eq!(last_turn(""), Turn::Going);
    }

    #[test]
    fn what_an_answer_asks() {
        assert_eq!(question("Done.\n\n**Which one do you want?**"), Some("Which one do you want?".into()));
        assert_eq!(question("All open questions:\n- the release date\n- the icon"), Some("the release date".into()));
        // Question marks in URLs and code are not questions.
        assert_eq!(question("See https://x.dev/a?v=2 and\n```\nif a? {}\n```\nAll done."), None);
        assert_eq!(question("Built it. Tests pass."), None);
    }

    /// The live statuses get the transcript's word: a busy session whose turn is over is idle,
    /// one that asked is waiting with its question.
    #[test]
    fn statuses_from_turns() {
        let mut turns = Turns::default();
        let s = |status: &str| Session { pid: 1, id: "x".into(), name: "x".into(), named: true, interactive: true, vscode: false, title: String::new(), status: status.into(), waiting_for: String::new(), cwd: String::new(), since_ms: 5 };
        let path = std::env::temp_dir().join(format!("agentsignals-test-{}-turn.jsonl", std::process::id()));
        std::fs::write(&path, r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Ready. Deploy now?"}]}}"#).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        turns.seen.insert("x".into(), (path.clone(), meta.modified().unwrap(), meta.len(), None));
        // Seeded with no answer for that file state: read it again by changing what was seen.
        turns.seen.get_mut("x").unwrap().2 = 0;
        let mut live = HashMap::from([(1, s("busy"))]);
        turns.apply(&mut live);
        assert_eq!((live[&1].status.as_str(), live[&1].waiting_for.as_str()), ("waiting", "Ready. Deploy now?"));
        std::fs::write(&path, r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Deployed."}]}}"#).unwrap();
        let mut live = HashMap::from([(1, s("busy"))]);
        turns.apply(&mut live);
        assert_eq!(live[&1].status, "idle");
        let _ = std::fs::remove_file(path);
    }
}
