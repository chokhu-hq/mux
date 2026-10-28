use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use mux_core::protocol::Skill;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

// claude's skills and custom commands live as markdown files, personal ones first and then the project's
pub fn claude(project: &Path) -> Vec<Skill> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config =
        std::env::var_os("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
    let mut skills = Vec::new();
    for (root, scope) in [(config, "Personal"), (project.join(".claude"), "Project")] {
        // a skill is a folder holding SKILL.md, sometimes a few folders down when it was synced from an account
        let mut files = Vec::new();
        skill_files(&root.join("skills"), 4, &mut files);
        for file in files {
            let folder = file.parent().map(file_stem).unwrap_or_default();
            if let Some(skill) = from_file(&file, folder, scope) {
                skills.push(skill);
            }
        }
        for file in read_dir(&root.join("commands")) {
            if file.extension().is_some_and(|e| e == "md")
                && let Some(skill) = from_file(&file, file_stem(&file), scope)
            {
                skills.push(skill);
            }
        }
    }
    // a project skill with a personal one's name is the one claude runs there
    let mut seen = Vec::new();
    let mut kept: Vec<Skill> = skills
        .into_iter()
        .rev()
        .filter(|s| {
            let fresh = !seen.contains(&s.name);
            seen.push(s.name.clone());
            fresh
        })
        .collect();
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    kept
}

fn skill_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let file = dir.join("SKILL.md");
    if file.is_file() {
        out.push(file);
        return;
    }
    if depth == 0 {
        return;
    }
    for entry in read_dir(dir) {
        if entry.is_dir() {
            skill_files(&entry, depth - 1, out);
        }
    }
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn from_file(path: &Path, fallback: String, scope: &str) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let front = frontmatter(&text);
    // a skill can opt out of being typed by people, leaving it for claude to call on its own
    if front
        .iter()
        .any(|(k, v)| k == "user-invocable" && matches!(v.as_str(), "false" | "no" | "off"))
    {
        return None;
    }
    let field = |key: &str| front.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    Some(Skill {
        name: field("name").filter(|n| !n.is_empty()).unwrap_or(fallback),
        description: field("description").unwrap_or_default(),
        path: path.to_string_lossy().into_owned(),
        scope: scope.to_owned(),
    })
}

// the `key: value` pairs between the opening `---` lines, with quotes and folded lines undone
fn frontmatter(text: &str) -> Vec<(String, String)> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Vec::new();
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        match line.split_once(':') {
            Some((key, value)) if !indented && !key.contains(' ') => {
                let value = value.trim();
                let value = if matches!(value, ">" | "|" | ">-" | "|-") {
                    ""
                } else {
                    value
                };
                pairs.push((key.trim().to_owned(), unquote(value)));
            }
            // a folded or wrapped value continues on indented lines
            _ if indented => {
                if let Some((_, value)) = pairs.last_mut() {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(line.trim());
                }
            }
            _ => {}
        }
    }
    pairs
}

fn unquote(value: &str) -> String {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

// codex lists its skills for a folder through a short lived app-server
pub async fn codex(project: &Path) -> io::Result<Vec<Skill>> {
    let list = async {
        let mut child = Command::new("codex")
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let init = json!({ "id": 0, "method": "initialize", "params": { "clientInfo": { "name": "mux", "version": mux_core::VERSION } } });
        let list = json!({ "id": 1, "method": "skills/list", "params": { "cwds": [project] } });
        for msg in [init, json!({ "method": "initialized" }), list] {
            stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        }
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg["id"].as_u64() == Some(1) {
                return Ok(parse_codex(&msg["result"]));
            }
        }
        Err(io::Error::other("codex exited before listing skills"))
    };
    tokio::time::timeout(Duration::from_secs(20), list)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "took too long to list skills"))?
}

fn parse_codex(result: &Value) -> Vec<Skill> {
    let mut skills: Vec<Skill> = result["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| entry["skills"].as_array().cloned().unwrap_or_default())
        .filter(|s| s["enabled"] != false)
        .filter_map(|s| {
            // the short description fits a menu row, the full one is written for the model
            let description = s["interface"]["shortDescription"]
                .as_str()
                .or(s["shortDescription"].as_str())
                .or(s["description"].as_str())
                .unwrap_or_default();
            let scope = match s["scope"].as_str().unwrap_or_default() {
                "user" => "Personal",
                "repo" => "Project",
                "system" => "Built in",
                other => other,
            };
            Some(Skill {
                name: s["name"].as_str()?.to_owned(),
                description: description.to_owned(),
                path: s["path"].as_str().unwrap_or_default().to_owned(),
                scope: scope.to_owned(),
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills.dedup_by(|a, b| a.name == b.name);
    skills
}

// where `$name` mentions of known skills sit in a message, as byte ranges
pub fn mentions<'a>(text: &str, skills: &'a [Skill]) -> Vec<(usize, usize, &'a Skill)> {
    let mut found = Vec::new();
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_whitespace() {
            if let Some(from) = start.take()
                && let Some(name) = text[from..i].strip_prefix('$')
                && let Some(skill) = skills.iter().find(|s| s.name == name)
            {
                found.push((from, i, skill));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    found
}

// claude only expands a skill when `/name` starts a text block, so the last one mentioned opens the final block
// and anything written before it goes in its own block first, with each mention turned into `/name`
pub fn claude_blocks(text: &str, skills: &[Skill]) -> Vec<String> {
    let found = mentions(text, skills);
    let Some(&(start, end, last)) = found.last() else {
        return vec![text.to_owned()];
    };
    let mut blocks = Vec::new();
    if !text[..start].trim().is_empty() {
        let mut leading = String::new();
        let mut cursor = 0;
        for &(from, to, skill) in &found {
            leading.push_str(&text[cursor..from]);
            leading.push('/');
            leading.push_str(&skill.name);
            cursor = to;
        }
        leading.push_str(&text[cursor..]);
        blocks.push(leading.trim_end().to_owned());
    }
    blocks.push(
        format!("/{}{}", last.name, &text[end..])
            .trim_end()
            .to_owned(),
    );
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> Skill {
        Skill {
            name: name.into(),
            description: String::new(),
            path: format!("/s/{name}/SKILL.md"),
            scope: "Personal".into(),
        }
    }

    #[test]
    fn reads_names_and_folded_descriptions_from_frontmatter() {
        let text = "---\nname: code-review\ndescription: >\n  Review the diff\n  for bugs\nuser-invocable: true\n---\n# body";
        let pairs = frontmatter(text);
        assert!(pairs.contains(&("name".into(), "code-review".into())));
        assert!(pairs.contains(&("description".into(), "Review the diff for bugs".into())));
        assert_eq!(frontmatter("# no frontmatter"), Vec::new());
    }

    #[test]
    fn finds_skill_mentions_but_not_prices_or_strangers() {
        let skills = [skill("debug"), skill("code-review")];
        let found = mentions("use $debug then $code-review, not $5 or $nope", &skills);
        let names: Vec<&str> = found.iter().map(|(_, _, s)| s.name.as_str()).collect();
        assert_eq!(names, ["debug"]);
        let found = mentions("$code-review the auth module", &skills);
        assert_eq!(found[0].2.name, "code-review");
    }

    #[test]
    fn claude_gets_the_last_skill_at_the_start_of_its_own_block() {
        let skills = [skill("debug"), skill("review")];
        assert_eq!(
            claude_blocks("$review the auth module", &skills),
            ["/review the auth module"]
        );
        assert_eq!(
            claude_blocks("first $debug the crash, then $review it", &skills),
            ["first /debug the crash, then /review it", "/review it"]
        );
        assert_eq!(claude_blocks("no skills here", &skills), ["no skills here"]);
    }

    #[test]
    fn reads_codex_skill_lists() {
        let result = json!({ "data": [{ "cwd": "/p", "skills": [
            { "name": "imagegen", "description": "long", "interface": { "shortDescription": "Make images" },
              "path": "/s/imagegen/SKILL.md", "scope": "system", "enabled": true },
            { "name": "off", "description": "x", "path": "/s/off", "scope": "user", "enabled": false }
        ] }] });
        let skills = parse_codex(&result);
        assert_eq!(skills.len(), 1);
        assert_eq!(
            (skills[0].description.as_str(), skills[0].scope.as_str()),
            ("Make images", "Built in")
        );
    }
}
