use super::command::{imported, quote_for};
use super::*;

#[test]
fn command_lines_split_like_a_shell() {
    assert_eq!(
        split_for(r#"npx -y "@scope/pkg" --dir 'My Notes' a\ b """#, false),
        ["npx", "-y", "@scope/pkg", "--dir", "My Notes", "a b", ""]
    );
    assert_eq!(
        split_for(
            r#"C:\tools\server.exe "C:\My Files\x" \\host\share say\"hi "C:\dir\\" next it's"#,
            true
        ),
        [
            r"C:\tools\server.exe",
            r"C:\My Files\x",
            r"\\host\share",
            r#"say"hi"#,
            r"C:\dir\",
            "next",
            "it's"
        ]
    );
}

#[test]
fn saved_commands_read_back_word_for_word() {
    let words = [
        "uvx",
        "with space",
        "it's",
        r"a\b",
        r"C:\dir\",
        r"C:\My Files\",
        r#"q"uote"#,
        r#"a\"b"#,
        r"\\host\share",
        "",
        r"trailing\\",
    ]
    .map(str::to_owned);
    for windows in [false, true] {
        let line = words
            .iter()
            .map(|w| quote_for(w, windows))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            split_for(&line, windows),
            words,
            "windows: {windows}, line: {line}"
        );
    }
}

#[test]
fn readme_json_imports_in_any_of_its_usual_shapes() {
    let full = r#"{ "mcpServers": {
        "fs": { "command": "npx", "args": ["-y", "server-fs", "/tmp"], "env": { "K": "v" } },
        "web": { "type": "http", "url": "https://x.dev/mcp", "headers": { "Authorization": "Bearer t" } }
    } }"#;
    let servers = imported(full).unwrap();
    assert_eq!(servers.len(), 2);
    assert!(
        matches!(&servers[0].transport, McpTransport::Stdio { args, env, .. }
        if args.len() == 3 && env[0] == ("K".into(), "v".into()))
    );
    assert!(
        matches!(&servers[1].transport, McpTransport::Http { url, .. } if url == "https://x.dev/mcp")
    );
    let bare = r#""fs": { "command": "uvx", "args": ["mcp-fs"] },"#;
    assert_eq!(imported(bare).unwrap()[0].name, "fs");
    assert!(imported("{}").is_err() && imported("nope").is_err());
    let sse = r#"{ "old": { "type": "sse", "url": "https://x.dev/sse" } }"#;
    assert!(imported(sse).unwrap_err().contains("SSE"));
    let spaced = r#"{ "my server": { "command": "x" } }"#;
    assert!(imported(spaced).unwrap_err().contains("my server"));
}

#[test]
fn drafts_check_what_was_typed() {
    let row = |k: &str, v: &str| (k.to_owned(), v.to_owned());
    let draft = Draft {
        name: "fs".into(),
        command: "npx -y server".into(),
        env: vec![row("A", "1"), row("", ""), row(" B ", " two ")],
        ..Default::default()
    };
    let server = draft.server(true).unwrap();
    let McpTransport::Stdio { command, args, env } = &server.transport else {
        panic!("expected a command");
    };
    assert_eq!((command.as_str(), args.len()), ("npx", 2));
    assert_eq!(env.len(), 2, "the blank row is skipped");
    assert_eq!(env[1], ("B".into(), "two".into()));
    let again = Draft::from(&server, false).server(true).unwrap();
    assert_eq!(again, server);
    let nameless = Draft {
        name: "x".into(),
        env: vec![row("", "secret")],
        command: "a".into(),
        ..Default::default()
    };
    assert!(nameless.server(true).is_err());
    let dotted = Draft {
        name: "a.b".into(),
        command: "a".into(),
        ..Default::default()
    };
    assert!(dotted.server(true).is_err());
}

#[test]
fn presets_make_valid_servers() {
    for preset in PRESETS {
        let draft = preset.draft(false);
        assert!(draft.open);
        assert!(draft.server(true).is_ok(), "{}", preset.name);
    }
}
