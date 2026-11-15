use std::sync::OnceLock;

const ISSUES: &str = "https://github.com/chokhu-hq/mux/issues/new";
// browsers and github turn away much longer links, so the panic is cut to fit
const MAX_URL: usize = 7000;
const MARKER: &str = "crashed";

// the gpu this run draws with, for a crash report
pub(crate) static GPU: OnceLock<String> = OnceLock::new();

// what the last run panicked with
pub(crate) struct Crash {
    pub build: String,
    pub panic: String,
}

// a panic is noted beside the log, so the next start can offer to report it
pub(crate) fn watch() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let note = format!("{}\n{info}\n{backtrace}", crate::update::build());
        let _ = mux_core::write_private(&marker(), note.as_bytes());
        previous(info);
    }));
}

fn marker() -> std::path::PathBuf {
    mux_core::protocol::data_dir().join(MARKER)
}

// the last run's panic, taken so it's offered once
pub(crate) fn take() -> Option<Crash> {
    let note = std::fs::read_to_string(marker()).ok()?;
    let _ = std::fs::remove_file(marker());
    let (build, panic) = note.split_once('\n')?;
    Some(Crash {
        build: build.to_owned(),
        panic: panic.trim().to_owned(),
    })
}

// a new github issue filled in with the build, the gpu and the panic, but not the log,
// since the log can name more than redaction catches, and the person should look it over before sharing it
pub(crate) fn issue_url(crash: &Crash, private: &crate::redact::Redactor) -> String {
    let panic = private.apply(&crash.panic);
    let message = panic
        .lines()
        .nth(1)
        .unwrap_or("panic")
        .trim()
        .chars()
        .take(80)
        .collect::<String>();
    let title = format!("Crash: {message}");
    // frames without a name say nothing, and there are usually dozens of them
    let mut panic: Vec<&str> = panic
        .lines()
        .filter(|l| !l.trim_end().ends_with("<unknown>"))
        .collect();
    loop {
        let body = body(crash, &panic.join("\n"));
        let url = format!("{ISSUES}?title={}&body={}", encode(&title), encode(&body));
        if url.len() <= MAX_URL || panic.len() <= 2 {
            return url;
        }
        panic.pop();
    }
}

fn body(crash: &Crash, panic: &str) -> String {
    let gpu = GPU.get().map_or("unknown", String::as_str);
    format!(
        "**What happened**\n\n<!-- What were you doing when Mux crashed? -->\n\n\
         **Build:** Mux {} ({}) on {} {}\n**GPU:** {gpu}\n\n\
         **Panic**\n\n```\n{panic}\n```\n\n\
         <!-- The full log is on your clipboard. Folders, names, emails and keys were taken out, but check it before pasting it here. -->\n",
        mux_core::VERSION,
        crash.build,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

// percent encoding for a link's query, keeping only what never needs escaping
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crash(frames: usize) -> Crash {
        let mut panic =
            "panicked at src/renderer.rs:1033:17:\nFailed to create staging buffer\n".to_owned();
        for i in 0..frames {
            panic.push_str(&format!("  {i}: <unknown>\n  {i}: mux_gui::frame_{i}\n"));
        }
        Crash {
            build: "experimental abc1234".to_owned(),
            panic,
        }
    }

    #[test]
    fn the_issue_names_the_panic_and_leaves_out_unnamed_frames() {
        let url = issue_url(&crash(3), &crate::redact::Redactor::here(&[], &[]));
        assert!(url.starts_with(
            "https://github.com/chokhu-hq/mux/issues/new?title=Crash%3A%20Failed%20to%20create"
        ));
        assert!(url.contains("experimental%20abc1234"));
        assert!(url.contains("mux_gui%3A%3Aframe_2"));
        assert!(!url.contains("unknown%3E"));
    }

    #[test]
    fn a_long_panic_is_cut_to_fit_a_link() {
        let url = issue_url(&crash(2000), &crate::redact::Redactor::here(&[], &[]));
        assert!(url.len() <= MAX_URL);
        assert!(url.contains("frame_0"));
    }

    #[test]
    fn encoding_escapes_everything_but_plain_characters() {
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }
}
