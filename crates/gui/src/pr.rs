use mux_core::protocol::PrLink;
use serde::{Deserialize, Deserializer};

const VIEW_FIELDS: &str = "number,title,body,state,isDraft,mergeable,reviewDecision,statusCheckRollup,reviews,comments,files,labels,url,headRefName,baseRefName,additions,deletions";
const LIST_FIELDS: &str =
    "number,title,url,author,headRefName,isDraft,reviewDecision,statusCheckRollup,updatedAt";

// a pull request as the panel shows it, read from gh
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Pr {
    pub number: u64,
    pub title: String,
    pub body: String,
    // OPEN, CLOSED or MERGED
    pub state: String,
    pub draft: bool,
    // MERGEABLE, CONFLICTING or UNKNOWN
    pub mergeable: String,
    // APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty
    pub decision: String,
    pub checks: Vec<Check>,
    pub reviews: Vec<Note>,
    pub comments: Vec<Note>,
    pub inline: Vec<Inline>,
    pub files: usize,
    pub labels: Vec<String>,
    pub url: String,
    pub head: String,
    pub base: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckState {
    Passed,
    Failed,
    Pending,
    Skipped,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Check {
    pub name: String,
    // the workflow it ran in, empty for a plain commit status
    pub workflow: String,
    pub state: CheckState,
    // what github calls the status or conclusion, like `in progress` or `timed out`
    pub detail: String,
    pub url: Option<String>,
}

// a review or a conversation comment
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Note {
    pub author: String,
    // unix seconds, 0 when github left it out
    pub when: u64,
    pub body: String,
    // APPROVED, CHANGES_REQUESTED or COMMENTED for reviews, empty for comments
    pub state: String,
}

// a review comment on a line of the diff
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Inline {
    pub author: String,
    pub when: u64,
    pub body: String,
    pub path: String,
    pub line: Option<usize>,
    pub start: Option<usize>,
    // RIGHT for the new side of the diff, LEFT for the old
    pub side: String,
    pub hunk: String,
    pub reply: bool,
}

impl Inline {
    // where it sits, like `src/a.rs:12` or `src/a.rs:10-12`
    pub fn place(&self) -> String {
        match (self.start, self.line) {
            (Some(start), Some(line)) if start != line => {
                format!("{}:{start}-{line}", self.path)
            }
            (_, Some(line)) => format!("{}:{line}", self.path),
            _ => self.path.clone(),
        }
    }
}

// one row of the project's open pull requests
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Summary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub head: String,
    pub draft: bool,
    pub decision: String,
    pub checks: Vec<Check>,
    pub updated: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    All,
    Mine,
    Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Merge {
    Squash,
    Commit,
    Rebase,
}

impl Merge {
    pub const ALL: [(Merge, &'static str, &'static str); 3] = [
        (
            Merge::Squash,
            "Squash and merge",
            "One commit on the base branch",
        ),
        (
            Merge::Commit,
            "Create a merge commit",
            "Keep every commit and join them with a merge",
        ),
        (
            Merge::Rebase,
            "Rebase and merge",
            "Put each commit on the base branch",
        ),
    ];

    fn flag(self) -> &'static str {
        match self {
            Merge::Squash => "--squash",
            Merge::Commit => "--merge",
            Merge::Rebase => "--rebase",
        }
    }
}

// something the panel asks gh to change on github
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Write {
    Ready,
    Merge(Merge),
    Approve(String),
    RequestChanges(String),
    Comment(String),
}

impl Write {
    fn args(&self, number: u64) -> Vec<String> {
        let n = number.to_string();
        let with_body = |mut args: Vec<String>, body: &str| {
            if !body.trim().is_empty() {
                args.extend(["--body".to_owned(), body.trim().to_owned()]);
            }
            args
        };
        let words = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        match self {
            Write::Ready => words(&["pr", "ready", &n]),
            Write::Merge(how) => words(&["pr", "merge", &n, how.flag()]),
            Write::Approve(body) => with_body(words(&["pr", "review", &n, "--approve"]), body),
            Write::RequestChanges(body) => {
                with_body(words(&["pr", "review", &n, "--request-changes"]), body)
            }
            Write::Comment(body) => with_body(words(&["pr", "comment", &n]), body),
        }
    }
}

// gh runs where the server does, in the session's checkout, so it finds the same repository
fn gh(dir: &str, args: &[&str]) -> Result<String, String> {
    let output = mux_core::paths::command_in("gh", dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => {
                "GitHub's gh isn't installed, so Mux can't read pull requests.".to_owned()
            }
            _ => format!("couldn't run gh: {err}"),
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr.trim();
    Err(if reason.is_empty() {
        format!("gh {} failed without saying why", args.join(" "))
    } else {
        reason.to_owned()
    })
}

// the pull request for a branch that was just pushed: a new one, the one already open, or none without gh
pub(crate) fn open_for(dir: &str, branch: &str) -> Option<PrLink> {
    if let Err(reason) = gh(dir, &["auth", "status"]) {
        log::info!("not opening a pull request with gh: {reason}");
        return None;
    }
    // gh names the existing pull request's address when it refuses to open a second one
    let created = match gh(dir, &["pr", "create", "--fill", "--head", branch]) {
        Ok(out) => out,
        Err(reason) => reason,
    };
    if let Some(link) = link_in(&created) {
        return Some(link);
    }
    let found = gh(dir, &["pr", "view", "--json", "url,number"]).ok()?;
    let found: LinkJson = serde_json::from_str(&found).ok()?;
    Some(PrLink {
        number: found.number,
        url: found.url,
    })
}

pub(crate) fn view(dir: &str, number: u64) -> Result<Pr, String> {
    let n = number.to_string();
    let mut pr = parse_view(&gh(dir, &["pr", "view", &n, "--json", VIEW_FIELDS])?)?;
    let path = format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments?per_page=100");
    // the inline threads are extra, so the rest still shows when they can't be read
    match gh(dir, &["api", &path]).and_then(|json| parse_inline(&json)) {
        Ok(inline) => pr.inline = inline,
        Err(reason) => log::warn!("couldn't read the inline comments on #{n}: {reason}"),
    }
    Ok(pr)
}

pub(crate) fn list(dir: &str, filter: Filter) -> Result<Vec<Summary>, String> {
    let mut args = vec!["pr", "list", "--json", LIST_FIELDS];
    match filter {
        Filter::All => {}
        Filter::Mine => args.extend(["--search", "author:@me"]),
        Filter::Review => args.extend(["--search", "review-requested:@me"]),
    }
    parse_list(&gh(dir, &args)?)
}

pub(crate) fn write(dir: &str, number: u64, action: &Write) -> Result<(), String> {
    let args = action.args(number);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    gh(dir, &args).map(|_| ())
}

// the first pull request address in some text, like gh's output after opening one
pub(crate) fn link_in(text: &str) -> Option<PrLink> {
    text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | '.' | ',')))
        .find_map(parse_url)
}

// `https://github.com/owner/repo/pull/12`, with anything after the number ignored
pub(crate) fn parse_url(url: &str) -> Option<PrLink> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let mut parts = rest.split('/');
    let (_host, owner, repo, pull, number) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    if owner.is_empty() || repo.is_empty() || pull != "pull" {
        return None;
    }
    let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
    let number = digits.parse().ok()?;
    let base = &url[..url.find("/pull/")? + "/pull/".len()];
    Some(PrLink {
        number,
        url: format!("{base}{number}"),
    })
}

// unix seconds from github's `2026-11-10T08:30:00Z`, 0 when it isn't one
pub(crate) fn parse_time(text: &str) -> u64 {
    let field = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let parts = (
        field(0..4),
        field(5..7),
        field(8..10),
        field(11..13),
        field(14..16),
        field(17..19),
    );
    let (Some(y), Some(m), Some(d), Some(h), Some(min), Some(s)) = parts else {
        return 0;
    };
    // days from the civil calendar, after howard hinnant's algorithm
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + s).unwrap_or(0)
}

// github sends null for lists and text it has nothing for
fn nullable<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct LinkJson {
    number: u64,
    url: String,
}

#[derive(Default, Deserialize)]
struct Author {
    #[serde(default, deserialize_with = "nullable")]
    login: String,
}

fn login(author: Option<Author>) -> String {
    author
        .map(|a| a.login)
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "ghost".to_owned())
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ViewJson {
    number: u64,
    #[serde(deserialize_with = "nullable")]
    title: String,
    #[serde(deserialize_with = "nullable")]
    body: String,
    #[serde(deserialize_with = "nullable")]
    state: String,
    is_draft: bool,
    #[serde(deserialize_with = "nullable")]
    mergeable: String,
    #[serde(deserialize_with = "nullable")]
    review_decision: String,
    #[serde(deserialize_with = "nullable")]
    status_check_rollup: Vec<CheckJson>,
    #[serde(deserialize_with = "nullable")]
    reviews: Vec<NoteJson>,
    #[serde(deserialize_with = "nullable")]
    comments: Vec<NoteJson>,
    #[serde(deserialize_with = "nullable")]
    files: Vec<serde_json::Value>,
    #[serde(deserialize_with = "nullable")]
    labels: Vec<LabelJson>,
    #[serde(deserialize_with = "nullable")]
    url: String,
    #[serde(deserialize_with = "nullable")]
    head_ref_name: String,
    #[serde(deserialize_with = "nullable")]
    base_ref_name: String,
    additions: u64,
    deletions: u64,
}

// a check run has a status and a conclusion, and an older commit status has one state
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CheckJson {
    #[serde(deserialize_with = "nullable")]
    name: String,
    #[serde(deserialize_with = "nullable")]
    context: String,
    #[serde(deserialize_with = "nullable")]
    workflow_name: String,
    #[serde(deserialize_with = "nullable")]
    status: String,
    #[serde(deserialize_with = "nullable")]
    conclusion: String,
    #[serde(deserialize_with = "nullable")]
    state: String,
    details_url: Option<String>,
    target_url: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct NoteJson {
    author: Option<Author>,
    #[serde(deserialize_with = "nullable")]
    body: String,
    #[serde(deserialize_with = "nullable")]
    state: String,
    #[serde(deserialize_with = "nullable")]
    submitted_at: String,
    #[serde(deserialize_with = "nullable")]
    created_at: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LabelJson {
    #[serde(deserialize_with = "nullable")]
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct InlineJson {
    user: Option<Author>,
    #[serde(deserialize_with = "nullable")]
    body: String,
    #[serde(deserialize_with = "nullable")]
    path: String,
    line: Option<usize>,
    original_line: Option<usize>,
    start_line: Option<usize>,
    #[serde(deserialize_with = "nullable")]
    side: String,
    #[serde(deserialize_with = "nullable")]
    diff_hunk: String,
    #[serde(deserialize_with = "nullable")]
    created_at: String,
    in_reply_to_id: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SummaryJson {
    number: u64,
    #[serde(deserialize_with = "nullable")]
    title: String,
    #[serde(deserialize_with = "nullable")]
    url: String,
    author: Option<Author>,
    #[serde(deserialize_with = "nullable")]
    head_ref_name: String,
    is_draft: bool,
    #[serde(deserialize_with = "nullable")]
    review_decision: String,
    #[serde(deserialize_with = "nullable")]
    status_check_rollup: Vec<CheckJson>,
    #[serde(deserialize_with = "nullable")]
    updated_at: String,
}

fn words(text: &str) -> String {
    text.to_ascii_lowercase().replace('_', " ")
}

fn check(raw: CheckJson) -> Check {
    let (name, url) = if raw.name.is_empty() {
        (raw.context, raw.target_url)
    } else {
        (raw.name, raw.details_url)
    };
    let (state, detail) = if !raw.state.is_empty() {
        let state = match raw.state.as_str() {
            "SUCCESS" => CheckState::Passed,
            "FAILURE" | "ERROR" => CheckState::Failed,
            _ => CheckState::Pending,
        };
        (state, words(&raw.state))
    } else if raw.status == "COMPLETED" {
        let state = match raw.conclusion.as_str() {
            "SUCCESS" => CheckState::Passed,
            "NEUTRAL" | "SKIPPED" | "STALE" => CheckState::Skipped,
            _ => CheckState::Failed,
        };
        (state, words(&raw.conclusion))
    } else {
        (CheckState::Pending, words(&raw.status))
    };
    Check {
        name,
        workflow: raw.workflow_name,
        state,
        detail,
        url: url.filter(|u| !u.is_empty()),
    }
}

fn note(raw: NoteJson) -> Note {
    let when = if raw.submitted_at.is_empty() {
        &raw.created_at
    } else {
        &raw.submitted_at
    };
    Note {
        when: parse_time(when),
        author: login(raw.author),
        body: raw.body,
        state: raw.state,
    }
}

pub(crate) fn parse_view(json: &str) -> Result<Pr, String> {
    let raw: ViewJson =
        serde_json::from_str(json).map_err(|err| format!("couldn't read gh's answer: {err}"))?;
    // a review with no words is only the wrapper for its inline comments, unless it approved or asked for changes
    let reviews = raw
        .reviews
        .into_iter()
        .map(note)
        .filter(|r| !r.body.trim().is_empty() || r.state != "COMMENTED")
        .collect();
    Ok(Pr {
        number: raw.number,
        title: raw.title,
        body: raw.body,
        state: raw.state,
        draft: raw.is_draft,
        mergeable: raw.mergeable,
        decision: raw.review_decision,
        checks: raw.status_check_rollup.into_iter().map(check).collect(),
        reviews,
        comments: raw.comments.into_iter().map(note).collect(),
        inline: Vec::new(),
        files: raw.files.len(),
        labels: raw.labels.into_iter().map(|l| l.name).collect(),
        url: raw.url,
        head: raw.head_ref_name,
        base: raw.base_ref_name,
        additions: raw.additions,
        deletions: raw.deletions,
    })
}

pub(crate) fn parse_inline(json: &str) -> Result<Vec<Inline>, String> {
    let raw: Vec<InlineJson> =
        serde_json::from_str(json).map_err(|err| format!("couldn't read gh's answer: {err}"))?;
    Ok(raw
        .into_iter()
        .map(|c| Inline {
            author: login(c.user),
            when: parse_time(&c.created_at),
            body: c.body,
            path: c.path,
            // an outdated comment has no line on the current diff, only where it first was
            line: c.line.or(c.original_line),
            start: c.start_line,
            side: c.side,
            hunk: c.diff_hunk,
            reply: c.in_reply_to_id.is_some(),
        })
        .collect())
}

pub(crate) fn parse_list(json: &str) -> Result<Vec<Summary>, String> {
    let raw: Vec<SummaryJson> =
        serde_json::from_str(json).map_err(|err| format!("couldn't read gh's answer: {err}"))?;
    Ok(raw
        .into_iter()
        .map(|s| Summary {
            number: s.number,
            title: s.title,
            url: s.url,
            author: login(s.author),
            head: s.head_ref_name,
            draft: s.is_draft,
            decision: s.review_decision,
            checks: s.status_check_rollup.into_iter().map(check).collect(),
            updated: parse_time(&s.updated_at),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pull_request_addresses() {
        let want = Some(PrLink {
            number: 12,
            url: "https://github.com/me/app/pull/12".to_owned(),
        });
        assert_eq!(parse_url("https://github.com/me/app/pull/12"), want);
        assert_eq!(parse_url("https://github.com/me/app/pull/12/files"), want);
        assert_eq!(
            parse_url("https://github.com/me/app/pull/12#discussion"),
            want
        );
        assert_eq!(parse_url("https://github.com/me/app/issues/12"), None);
        assert_eq!(parse_url("https://github.com/me/app/pull/"), None);
        assert_eq!(parse_url("github.com/me/app/pull/12"), None);
        assert_eq!(
            link_in("Creating pull request\nhttps://github.com/me/app/pull/12\n"),
            want
        );
        assert_eq!(
            link_in(
                "a pull request for branch \"b\" into branch \"main\" already exists:\nhttps://github.com/me/app/pull/12"
            ),
            want
        );
        assert_eq!(link_in("no pull requests found"), None);
    }

    #[test]
    fn reads_github_times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_time("2024-02-29T12:30:15Z"), 1_709_209_815);
        assert_eq!(parse_time(""), 0);
        assert_eq!(parse_time("soon"), 0);
    }

    #[test]
    fn reads_the_checks_rollup() {
        let pr = parse_view(
            r#"{"number":7,"title":"Fix","state":"OPEN","isDraft":true,"reviewDecision":"","statusCheckRollup":[
                {"__typename":"CheckRun","name":"test","workflowName":"CI","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://ci/1"},
                {"__typename":"CheckRun","name":"lint","workflowName":"CI","status":"IN_PROGRESS","conclusion":"","detailsUrl":"https://ci/2"},
                {"__typename":"CheckRun","name":"docs","status":"COMPLETED","conclusion":"SKIPPED","detailsUrl":""},
                {"__typename":"StatusContext","context":"deploy","state":"SUCCESS","targetUrl":"https://deploy"}
            ],"reviews":null,"comments":[],"files":[{"path":"a"},{"path":"b"}],"labels":[{"name":"bug"}],"mergeable":"MERGEABLE","additions":3,"deletions":1}"#,
        )
        .unwrap();
        let states: Vec<_> = pr
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            states,
            [
                ("test", CheckState::Failed),
                ("lint", CheckState::Pending),
                ("docs", CheckState::Skipped),
                ("deploy", CheckState::Passed),
            ]
        );
        assert_eq!(pr.checks[0].detail, "failure");
        assert_eq!(pr.checks[1].detail, "in progress");
        assert_eq!(pr.checks[0].workflow, "CI");
        assert_eq!(pr.checks[2].url, None);
        assert_eq!(pr.checks[3].url.as_deref(), Some("https://deploy"));
        assert!(pr.draft);
        assert_eq!((pr.files, pr.labels.clone()), (2, vec!["bug".to_owned()]));
        assert!(pr.reviews.is_empty());
    }

    #[test]
    fn reads_reviews_and_comments() {
        let pr = parse_view(
            r#"{"number":7,"reviews":[
                {"author":{"login":"ana"},"state":"CHANGES_REQUESTED","body":"Needs a test","submittedAt":"2024-02-29T12:30:15Z"},
                {"author":{"login":"bo"},"state":"COMMENTED","body":"","submittedAt":"2024-02-29T12:31:00Z"},
                {"author":{"login":"bo"},"state":"APPROVED","body":"","submittedAt":"2024-02-29T12:32:00Z"},
                {"author":null,"state":"COMMENTED","body":"Old account","submittedAt":null}
            ],"comments":[{"author":{"login":"cy"},"body":"Thanks!","createdAt":"2024-02-29T12:30:15Z"}]}"#,
        )
        .unwrap();
        let reviews: Vec<_> = pr
            .reviews
            .iter()
            .map(|r| (r.author.as_str(), r.state.as_str()))
            .collect();
        assert_eq!(
            reviews,
            [
                ("ana", "CHANGES_REQUESTED"),
                ("bo", "APPROVED"),
                ("ghost", "COMMENTED")
            ]
        );
        assert_eq!(pr.reviews[0].when, 1_709_209_815);
        assert_eq!(pr.reviews[2].when, 0);
        assert_eq!(pr.comments[0].author, "cy");
        assert_eq!(pr.comments[0].body, "Thanks!");
        assert_eq!(pr.comments[0].when, 1_709_209_815);
    }

    #[test]
    fn reads_inline_comments() {
        let inline = parse_inline(
            r#"[
                {"id":1,"user":{"login":"ana"},"body":"Rename this","path":"src/a.rs","line":12,"start_line":10,"side":"RIGHT","diff_hunk":"@@ -1,2 +1,3 @@\n+x","created_at":"2024-02-29T12:30:15Z","in_reply_to_id":null},
                {"id":2,"user":{"login":"bo"},"body":"Done","path":"src/a.rs","line":null,"original_line":4,"start_line":null,"side":"LEFT","diff_hunk":"","created_at":"2024-02-29T12:31:00Z","in_reply_to_id":1}
            ]"#,
        )
        .unwrap();
        assert_eq!(inline[0].place(), "src/a.rs:10-12");
        assert_eq!(inline[0].author, "ana");
        assert!(!inline[0].reply);
        assert_eq!(inline[1].place(), "src/a.rs:4");
        assert!(inline[1].reply);
        assert_eq!(inline[1].side, "LEFT");
        assert!(parse_inline("{\"message\":\"Not Found\"}").is_err());
    }

    #[test]
    fn reads_the_list() {
        let list = parse_list(
            r#"[{"number":3,"title":"Add x","url":"https://github.com/me/app/pull/3","author":{"login":"ana"},"headRefName":"ana/x","isDraft":false,"reviewDecision":"APPROVED","statusCheckRollup":[],"updatedAt":"2024-02-29T12:30:15Z"}]"#,
        )
        .unwrap();
        assert_eq!(list[0].number, 3);
        assert_eq!(list[0].author, "ana");
        assert_eq!(list[0].head, "ana/x");
        assert_eq!(list[0].updated, 1_709_209_815);
        assert!(list[0].checks.is_empty());
    }

    #[test]
    fn writes_become_gh_arguments() {
        assert_eq!(Write::Ready.args(4), ["pr", "ready", "4"]);
        assert_eq!(
            Write::Merge(Merge::Squash).args(4),
            ["pr", "merge", "4", "--squash"]
        );
        assert_eq!(
            Write::Approve(" ".to_owned()).args(4),
            ["pr", "review", "4", "--approve"]
        );
        assert_eq!(
            Write::RequestChanges("Add a test\n".to_owned()).args(4),
            [
                "pr",
                "review",
                "4",
                "--request-changes",
                "--body",
                "Add a test"
            ]
        );
        assert_eq!(
            Write::Comment("Hi".to_owned()).args(4),
            ["pr", "comment", "4", "--body", "Hi"]
        );
    }
}
