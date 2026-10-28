use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use mux_core::Provider;
use mux_core::protocol::{TokenTotals, UsageDay};
use serde_json::Value;

// how far back the settings page looks
const DAYS: i64 = 30;

// where each file was read up to, and the model codex last switched to in it
#[derive(Default)]
struct FileState {
    offset: u64,
    model: String,
}

// token use read from the transcripts codex and claude keep on disk, so it covers sessions run anywhere
// and only lines added since the last scan are read again
#[derive(Default)]
pub struct History {
    files: HashMap<PathBuf, FileState>,
    // a request can be recorded more than once, in claude's per block records and codex's forked copies
    seen: HashSet<String>,
    days: HashMap<(i64, Provider, String), TokenTotals>,
}

impl History {
    pub fn scan(&mut self) {
        for (provider, root) in roots() {
            let mut files = Vec::new();
            jsonl_files(&root, &mut files);
            for path in files {
                self.read(provider, &path);
            }
        }
    }

    // the last month by day, with today's local day number
    pub fn summary(&self) -> (i64, Vec<UsageDay>) {
        let today = local_day(now());
        let days = self
            .days
            .iter()
            .filter(|((day, _, _), _)| *day > today - DAYS)
            .map(|((day, provider, model), tokens)| UsageDay {
                day: *day,
                provider: *provider,
                model: model.clone(),
                tokens: *tokens,
            })
            .collect();
        (today, days)
    }

    fn read(&mut self, provider: Provider, path: &Path) {
        let Ok(mut file) = File::open(path) else {
            return;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let state = self.files.entry(path.to_owned()).or_default();
        // a file that shrank was rewritten, and anything already counted is skipped by its key
        if len < state.offset {
            *state = FileState::default();
        }
        if len == state.offset || file.seek(SeekFrom::Start(state.offset)).is_err() {
            return;
        }
        let mut model = std::mem::take(&mut state.model);
        let mut offset = state.offset;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut found = Vec::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                // a line still being written is left for the next scan
                Ok(_) if !line.ends_with('\n') => break,
                Ok(n) => offset += n as u64,
            }
            let record = match provider {
                Provider::Claude => claude_line(&line),
                Provider::Codex => codex_line(&line, &mut model),
                // opencode keeps its history in a database, not transcripts, so none are scanned
                Provider::OpenCode => None,
            };
            found.extend(record);
        }
        let state = self.files.entry(path.to_owned()).or_default();
        state.offset = offset;
        state.model = model;
        for record in found {
            if self.seen.insert(record.key) {
                self.days
                    .entry((local_day(record.at), provider, record.model))
                    .or_default()
                    .add(&record.tokens);
            }
        }
    }
}

struct Record {
    key: String,
    // unix seconds
    at: i64,
    model: String,
    tokens: TokenTotals,
}

fn roots() -> Vec<(Provider, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let codex = std::env::var_os("CODEX_HOME").map_or_else(|| home.join(".codex"), PathBuf::from);
    let claude =
        std::env::var_os("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    vec![
        (Provider::Codex, codex.join("sessions")),
        (Provider::Codex, codex.join("archived_sessions")),
        (Provider::Claude, claude.join("projects")),
    ]
}

fn jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => jsonl_files(&path, out),
            Ok(kind) if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") => {
                out.push(path)
            }
            _ => {}
        }
    }
}

// claude writes one record per content block and repeats the reply's usage on each, so the reply and request ids
// pick out one of them
fn claude_line(line: &str) -> Option<Record> {
    if !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
        return None;
    }
    let msg: Value = serde_json::from_str(line).ok()?;
    if msg["type"] != "assistant" {
        return None;
    }
    let message = &msg["message"];
    let model = message["model"].as_str()?;
    // placeholders claude writes locally never reached the api
    if model == "<synthetic>" {
        return None;
    }
    let usage = &message["usage"];
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    Some(Record {
        key: format!(
            "claude:{}:{}",
            message["id"].as_str()?,
            msg["requestId"].as_str().unwrap_or_default()
        ),
        at: timestamp(msg["timestamp"].as_str()?)?,
        model: model.to_owned(),
        tokens: TokenTotals {
            input: count("input_tokens"),
            cached: count("cache_read_input_tokens"),
            cache_write: count("cache_creation_input_tokens"),
            output: count("output_tokens"),
            reasoning: usage["output_tokens_details"]["thinking_tokens"]
                .as_u64()
                .unwrap_or(0),
        },
    })
}

// codex records each response's usage under its response id, and names the model in the turn context before it
fn codex_line(line: &str, model: &mut String) -> Option<Record> {
    let usage_record = line.contains("\"token_usage_record\"");
    if !usage_record && !line.contains("\"turn_context\"") {
        return None;
    }
    let msg: Value = serde_json::from_str(line).ok()?;
    let payload = &msg["payload"];
    if msg["type"] == "turn_context" {
        if let Some(name) = payload["model"].as_str() {
            *model = name.to_owned();
        }
        return None;
    }
    if msg["type"] != "token_usage_record" {
        return None;
    }
    let usage = &payload["usage"];
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let (cached, cache_write) = (
        count("cached_input_tokens"),
        count("cache_write_input_tokens"),
    );
    Some(Record {
        key: format!("codex:{}", payload["response_id"].as_str()?),
        at: timestamp(msg["timestamp"].as_str()?)?,
        model: if model.is_empty() {
            "unknown".to_owned()
        } else {
            model.clone()
        },
        tokens: TokenTotals {
            // codex counts cache reads and writes inside its input total
            input: count("input_tokens").saturating_sub(cached + cache_write),
            cached,
            cache_write,
            output: count("output_tokens"),
            reasoning: count("reasoning_output_tokens"),
        },
    })
}

// reads `2026-10-21T14:48:19.434Z` as unix seconds
fn timestamp(text: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

// days since 1970-01-01 for a gregorian date, after Howard Hinnant's algorithm
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// the local calendar day a moment falls on, using the offset in force at that moment so summer time is right
fn local_day(at: i64) -> i64 {
    let time: libc::time_t = at as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r only writes into the struct it's given
    let offset = if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    };
    (at + offset).div_euclid(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_timestamps() {
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(timestamp("2026-10-21T14:48:19.434Z"), Some(1_792_594_099));
    }

    #[test]
    fn claude_replies_count_once_and_placeholders_not_at_all() {
        let line = r#"{"type":"assistant","requestId":"r1","timestamp":"2026-10-21T14:48:19.434Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":8550,"cache_read_input_tokens":14135,"output_tokens":79}}}"#;
        let record = claude_line(line).unwrap();
        assert_eq!(record.key, "claude:m1:r1");
        assert_eq!(record.tokens.total(), 2 + 8550 + 14135 + 79);
        let synthetic = line.replace("claude-opus-5-5", "<synthetic>");
        assert!(claude_line(&synthetic).is_none());
    }

    #[test]
    fn codex_usage_takes_the_turns_model_and_splits_out_the_cache() {
        let mut model = String::new();
        let context = r#"{"timestamp":"2026-10-22T15:14:00.000Z","type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#;
        assert!(codex_line(context, &mut model).is_none());
        let usage = r#"{"timestamp":"2026-10-22T15:14:20.122Z","type":"token_usage_record","payload":{"response_id":"resp_1","usage":{"input_tokens":15329,"cached_input_tokens":12032,"cache_write_input_tokens":0,"output_tokens":88,"reasoning_output_tokens":0,"total_tokens":15417}}}"#;
        let record = codex_line(usage, &mut model).unwrap();
        assert_eq!(record.model, "gpt-6.1-sol");
        assert_eq!(record.tokens.input, 15329 - 12032);
        assert_eq!(record.tokens.total(), 15417);
    }

    #[test]
    fn rescanning_only_reads_new_lines_and_never_counts_twice() {
        let dir = std::env::temp_dir().join(format!("mux-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let line = |id: &str| {
            format!(
                "{{\"type\":\"assistant\",\"requestId\":\"r\",\"timestamp\":\"2026-10-21T14:48:19Z\",\"message\":{{\"id\":\"{id}\",\"model\":\"m\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n"
            )
        };
        std::fs::write(&path, line("a") + &line("a")).unwrap();
        let mut history = History::default();
        history.read(Provider::Claude, &path);
        let total = |h: &History| h.days.values().map(TokenTotals::total).sum::<u64>();
        assert_eq!(total(&history), 2);
        // a half written line waits until it's finished
        std::fs::write(&path, line("a") + &line("a") + &line("b") + "{\"type\"").unwrap();
        history.read(Provider::Claude, &path);
        assert_eq!(total(&history), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
