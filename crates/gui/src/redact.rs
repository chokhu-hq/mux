// strips what a log names about the person before it's shared: their folders, name, machine,
// emails and anything shaped like a key. it can't know everything an agent quoted, so people still look it over
pub(crate) struct Redactor {
    // exact text to replace, longest first so a project inside the home folder is named as the project
    literals: Vec<(String, String)>,
    // names replaced only as whole words, since a short name would otherwise eat parts of other words
    words: Vec<(String, String)>,
}

const MIN_WORD: usize = 3;
const MIN_SECRET: usize = 6;

impl Redactor {
    // this machine's home folder, user and host, plus the project folders and saved secrets mux knows about
    pub fn here(folders: &[String], secrets: &[String]) -> Self {
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok();
        let host = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok());
        let home = mux_core::home_dir().to_string_lossy().into_owned();
        Self::new(Some(home), user, host, folders, secrets)
    }

    fn new(
        home: Option<String>,
        user: Option<String>,
        host: Option<String>,
        folders: &[String],
        secrets: &[String],
    ) -> Self {
        let mut literals = Vec::new();
        for secret in secrets.iter().filter(|s| s.len() >= MIN_SECRET) {
            literals.push((secret.clone(), "<secret>".to_owned()));
        }
        let mut folders: Vec<&String> = folders.iter().filter(|f| f.len() > 1).collect();
        folders.sort();
        folders.dedup();
        for (i, folder) in folders.into_iter().enumerate() {
            for form in spellings(folder) {
                literals.push((form, format!("<project {}>", i + 1)));
            }
        }
        if let Some(home) = home.filter(|h| h.len() > 1) {
            for form in spellings(&home) {
                literals.push((form, "~".to_owned()));
            }
        }
        literals.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        let mut words = Vec::new();
        for (name, with) in [(user, "<user>"), (host, "<host>")] {
            if let Some(name) = name
                .map(|n| n.trim().to_owned())
                .filter(|n| n.len() >= MIN_WORD)
            {
                words.push((name, with.to_owned()));
            }
        }
        // a host named after its user goes as the host, not the user with a suffix
        words.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        Self { literals, words }
    }

    pub fn apply(&self, text: &str) -> String {
        // the whole search path lists every tool folder on the machine and adds nothing to a bug report
        let mut out: String = text
            .lines()
            .filter(|line| !line.contains(" PATH: "))
            .map(|line| format!("{line}\n"))
            .collect();
        for (from, to) in &self.literals {
            out = replace_ignoring_case(&out, from, to, false);
        }
        for (from, to) in &self.words {
            out = replace_ignoring_case(&out, from, to, true);
        }
        tokens(&out)
    }
}

// a windows folder can show up with either slash and in any case, so each spelling is looked for
fn spellings(path: &str) -> Vec<String> {
    let path = path.trim_end_matches(['/', '\\']);
    let mut forms = vec![path.to_owned()];
    if path.contains('\\') {
        forms.push(path.replace('\\', "/"));
        forms.push(path.replace('\\', "\\\\"));
    }
    forms
}

fn replace_ignoring_case(text: &str, from: &str, to: &str, whole_word: bool) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(&needle) {
        let start = at + found;
        let end = start + needle.len();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if whole_word
            && !(boundary(text[..start].chars().next_back())
                && boundary(text[end..].chars().next()))
        {
            out.push_str(&text[at..end]);
            at = end;
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(to);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

// emails and key-shaped words, found word by word
fn tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut after_bearer = false;
    let flush = |word: &mut String, out: &mut String, after_bearer: &mut bool| {
        if !word.is_empty() {
            let bearer = word.eq_ignore_ascii_case("bearer");
            out.push_str(&redact_word(word, *after_bearer));
            *after_bearer = bearer;
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || "\"'`<>()[]{},;".contains(c) {
            flush(&mut word, &mut out, &mut after_bearer);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out, &mut after_bearer);
    out
}

const KEY_PREFIXES: [&str; 10] = [
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "AKIA",
];
const KEY_NAMES: [&str; 5] = ["token", "secret", "password", "api_key", "apikey"];

fn redact_word(word: &str, after_bearer: bool) -> String {
    if after_bearer && word.len() >= 8 {
        return "<secret>".to_owned();
    }
    // name=value, where the name says it's a key
    if let Some((name, value)) = word.split_once('=') {
        let name_lower = name.to_ascii_lowercase();
        if !value.is_empty() && KEY_NAMES.iter().any(|k| name_lower.contains(k)) {
            return format!("{name}=<secret>");
        }
    }
    if word.len() >= 16 && KEY_PREFIXES.iter().any(|p| word.starts_with(p)) {
        return "<secret>".to_owned();
    }
    if is_email(word) {
        return "<email>".to_owned();
    }
    word.to_owned()
}

fn is_email(word: &str) -> bool {
    let word = word.trim_end_matches(['.', ':']);
    let Some((local, domain)) = word.split_once('@') else {
        return false;
    };
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || "._%+-".contains(c))
    };
    plain(local)
        && plain(domain)
        && domain.rsplit_once('.').is_some_and(|(name, tld)| {
            !name.is_empty() && tld.len() >= 2 && tld.chars().all(char::is_alphabetic)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor() -> Redactor {
        Redactor::new(
            Some(r"C:\Users\Alex".to_owned()),
            Some("Alex".to_owned()),
            Some("ALEX-DESKTOP".to_owned()),
            &[
                r"C:\Users\Alex\code\client-app".to_owned(),
                "/home/alex/work".to_owned(),
            ],
            &["hunter2-mcp-key".to_owned()],
        )
    }

    #[test]
    fn folders_become_the_home_folder_or_a_project() {
        let r = redactor();
        assert_eq!(
            r.apply(r"opened c:\users\alex\code\client-app\src\main.rs"),
            "opened <project 2>\\src\\main.rs\n"
        );
        assert_eq!(
            r.apply("log at C:/Users/Alex/AppData/Local/mux/app.log"),
            "log at ~/AppData/Local/mux/app.log\n"
        );
        assert_eq!(r.apply("cd /home/alex/work/api"), "cd <project 1>/api\n");
    }

    #[test]
    fn names_go_only_as_whole_words() {
        let r = redactor();
        assert_eq!(
            r.apply("alex on ALEX-DESKTOP, not alexandra"),
            "<user> on <host>, not alexandra\n"
        );
        assert_eq!(r.apply("/home/alex/.config"), "/home/<user>/.config\n");
    }

    #[test]
    fn the_path_line_is_dropped() {
        let r = redactor();
        let log = "a\nINFO  mux_gui: using windows' current PATH: C:\\tools;D:\\bin\nb\n";
        assert_eq!(r.apply(log), "a\nb\n");
    }

    #[test]
    fn emails_and_keys_are_hidden() {
        let r = redactor();
        assert_eq!(
            r.apply("signed in as someone@example.com."),
            "signed in as <email>\n"
        );
        assert_eq!(
            r.apply("Authorization: Bearer abcdef123456 sent"),
            "Authorization: Bearer <secret> sent\n"
        );
        assert_eq!(
            r.apply("key sk-proj-abcdefghijklmnop and GITHUB_TOKEN=ghx1 api_key=xyz"),
            "key <secret> and GITHUB_TOKEN=<secret> api_key=<secret>\n"
        );
        assert_eq!(r.apply("env hunter2-mcp-key here"), "env <secret> here\n");
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let r = redactor();
        let line = "panicked at src/renderer.rs:1033:17: build 3c47ba7 v0.1.0 a@b";
        assert_eq!(r.apply(line), format!("{line}\n"));
    }
}
