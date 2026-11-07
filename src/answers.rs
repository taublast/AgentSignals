//! A session's questions and Claude's answers, read from its transcript: what the "Answers"
//! panel lists, newest first, to find an answer lost far up in a long conversation.

use serde_json::Value;
use std::io::BufRead;

/// One question the user asked and the answer Claude ended that turn with.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub question: String,
    pub answer: String,
    /// When the question was asked, epoch ms (0: unknown).
    pub at_ms: u64,
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
                let at_ms = record.get("timestamp").and_then(Value::as_str).and_then(epoch_ms).unwrap_or(0);
                list.push(Answer { question: text.to_string(), answer: String::new(), at_ms });
            }
        } else if let Some(last) = list.last_mut() {
            last.answer = text.to_string();
        }
    }
    list.retain(|a| !a.answer.is_empty());
    list
}

/// "2026-11-06T06:41:04.738Z" (UTC, as Claude Code writes it) to epoch ms.
pub(crate) fn epoch_ms(text: &str) -> Option<u64> {
    let n = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // The fraction as milliseconds: ".5" is 500.
    let fraction: String = text.get(19..).and_then(|r| r.strip_prefix('.')).map_or(String::new(), |r| r.chars().take_while(char::is_ascii_digit).take(3).collect());
    let ms = format!("{fraction:0<3}").parse::<i64>().unwrap_or(0);
    // Days from 1970-01-01 to the date (Howard Hinnant's days_from_civil).
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms).ok()
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

/// The search's words, lower case, each with whether it may be the start of a longer word: only
/// the last one, while it is still being typed ("mac sign" finds "Mac" and "signed", not "machine").
fn terms(filter: &str) -> Vec<(String, bool)> {
    let open = !filter.ends_with(char::is_whitespace);
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    let last = words.len().saturating_sub(1);
    words.into_iter().enumerate().map(|(i, w)| (w, open && i == last)).collect()
}

/// The entries that hold every word of `filter` in their question or their answer, ignoring case:
/// the ones whose question has them all first, then the ones found in the answer, each in list order.
pub fn matching<'a>(list: &'a [Answer], filter: &str) -> Vec<(usize, &'a Answer)> {
    let terms = terms(filter);
    let (mut asked, answered): (Vec<_>, Vec<_>) = list
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            let (q, t) = (a.question.to_lowercase(), a.answer.to_lowercase());
            terms.iter().all(|(w, prefix)| find_word(&q, w, *prefix).is_some() || find_word(&t, w, *prefix).is_some())
        })
        .partition(|(_, a)| {
            let q = a.question.to_lowercase();
            terms.iter().all(|(w, prefix)| find_word(&q, w, *prefix).is_some())
        });
    asked.extend(answered);
    asked
}

/// Where `word` first stands as a word of `text` (both lower case), as a byte offset; with
/// `prefix`, also as the start of a longer word.
fn find_word(text: &str, word: &str, prefix: bool) -> Option<usize> {
    if word.is_empty() {
        return Some(0);
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(word).map(|(i, _)| i).find(|&i| edge(text[..i].chars().next_back()) && (prefix || edge(text[i + word.len()..].chars().next())))
}

/// The part of `text` around the first hit of `filter`'s first word, on one line, with "…" where
/// it was cut: what a search result shows so the hit is seen. `None` when no word is in `text`.
pub fn snippet(text: &str, filter: &str) -> Option<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    let at = terms(filter).iter().find_map(|(w, prefix)| find_word(&lower, w, *prefix))?;
    // Lower case can change a text's byte length (rare letters): start on the same character.
    let chars_before = lower[..at].chars().count();
    let from = chars_before.saturating_sub(30);
    // Start at a word, not in the middle of one.
    let start = flat.char_indices().nth(from).map_or(0, |(i, _)| i);
    let start = if from == 0 { 0 } else { flat[start..].find(' ').map_or(start, |s| start + s + 1) };
    Some(if start == 0 { flat } else { format!("…{}", &flat[start..]) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_and_their_final_answers() {
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","timestamp":"2026-11-06T06:41:04.738Z","message":{"role":"user","content":"how do I deploy?"}}"#,
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
                Answer { question: "how do I deploy?".into(), answer: "Run dev/deploy.ps1.".into(), at_ms: 1_793_947_264_738 },
                Answer { question: "and the logs?".into(), answer: "In C:\\logs.".into(), at_ms: 0 },
            ]
        );
        let found = matching(&list, "DEPLOY run");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 0);
        assert_eq!(matching(&list, "").len(), 2);
        // Words match from their start, the question's matches come first.
        let list = [
            Answer { question: "and the design?".into(), answer: "Signed by the Mac.".into(), at_ms: 0 },
            Answer { question: "how do we sign the build?".into(), answer: "With the certificate.".into(), at_ms: 0 },
            Answer { question: "fonts".into(), answer: "Assign them in the redesign.".into(), at_ms: 0 },
        ];
        assert_eq!(matching(&list, "sign").iter().map(|m| m.0).collect::<Vec<_>>(), [1, 0]);
        // A finished word (a space after it) matches whole words only.
        assert_eq!(matching(&list, "sign ").iter().map(|m| m.0).collect::<Vec<_>>(), [1]);
        let mac = [Answer { question: "q".into(), answer: "on your machine".into(), at_ms: 0 }, Answer { question: "q".into(), answer: "on the Mac, signed".into(), at_ms: 0 }];
        assert_eq!(matching(&mac, "mac sign").iter().map(|m| m.0).collect::<Vec<_>>(), [1]);
        let long = "First a long introduction that goes on and on about nothing much at all, then the release notes come.";
        assert_eq!(snippet(long, "release").unwrap(), "…much at all, then the release notes come.");
        assert_eq!(snippet("Release first.", "release").unwrap(), "Release first.");
        assert_eq!(snippet("nothing here", "release"), None);
        assert_eq!(epoch_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(epoch_ms("2000-03-01T00:00:00.5Z"), Some(951_868_800_500));
    }
}
