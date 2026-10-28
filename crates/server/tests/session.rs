// these talk to the server over its unix socket
#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use mux_core::SessionKind;
use mux_core::protocol::{self, AgentItem, ClientMsg, PROTOCOL, ServerMsg};

#[test]
fn shell_round_trip() {
    let dir = std::env::temp_dir().join(format!("mux-it-{}", std::process::id()));
    let socket = dir.join("mux.sock");
    let state = dir.join("state.json");
    let server = {
        let (socket, state) = (socket.clone(), state.clone());
        std::thread::spawn(move || mux_server::run_at(socket, state))
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("server never came up: {err}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let send = |stream: &mut UnixStream, msg| protocol::write(stream, &msg).unwrap();
    let recv = |stream: &mut UnixStream| protocol::read::<ServerMsg>(stream).unwrap();

    send(&mut stream, ClientMsg::Hello { protocol: PROTOCOL });
    assert!(matches!(
        recv(&mut stream),
        ServerMsg::Welcome { protocol: PROTOCOL }
    ));

    send(
        &mut stream,
        ClientMsg::CreateSession {
            group: None,
            title: "test shell".to_owned(),
            kind: SessionKind::Shell,
        },
    );
    let id = loop {
        if let ServerMsg::Created { id } = recv(&mut stream) {
            break id;
        }
    };
    send(&mut stream, ClientMsg::Attach { id });
    send(
        &mut stream,
        ClientMsg::Input {
            id,
            data: b"echo mux-$((40+2))\r".to_vec(),
        },
    );

    let mut output = Vec::new();
    let found = loop {
        assert!(
            Instant::now() < deadline + Duration::from_secs(5),
            "no echo in {:?}",
            String::from_utf8_lossy(&output)
        );
        match recv(&mut stream) {
            ServerMsg::Output { data, .. } | ServerMsg::Attached { replay: data, .. } => {
                output.extend(data)
            }
            _ => {}
        }
        if String::from_utf8_lossy(&output).contains("mux-42") {
            break true;
        }
    };
    assert!(found);
    assert!(state.exists(), "state was never saved");

    send(&mut stream, ClientMsg::Shutdown);
    server.join().unwrap().unwrap();
    assert!(!socket.exists(), "socket left behind");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn shell_starts_in_group_folder() {
    let dir = std::env::temp_dir().join(format!("mux-it-folder-{}", std::process::id()));
    let project = dir.join("project-folder");
    std::fs::create_dir_all(&project).unwrap();
    let socket = dir.join("mux.sock");
    let server = {
        let (socket, state) = (socket.clone(), dir.join("state.json"));
        std::thread::spawn(move || mux_server::run_at(socket, state))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("server never came up: {err}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let send = |stream: &mut UnixStream, msg| protocol::write(stream, &msg).unwrap();
    let recv = |stream: &mut UnixStream| protocol::read::<ServerMsg>(stream).unwrap();

    send(&mut stream, ClientMsg::Hello { protocol: PROTOCOL });
    send(
        &mut stream,
        ClientMsg::CreateGroup {
            name: "project".to_owned(),
            path: Some(project.to_string_lossy().into_owned()),
        },
    );
    let group = loop {
        if let ServerMsg::State { groups } = recv(&mut stream)
            && let Some(group) = groups.iter().find(|g| g.name == "project")
        {
            break group.id;
        }
    };
    send(
        &mut stream,
        ClientMsg::CreateSession {
            group: Some(group),
            title: "pwd".to_owned(),
            kind: SessionKind::Task,
        },
    );
    let id = loop {
        if let ServerMsg::Created { id } = recv(&mut stream) {
            break id;
        }
    };
    send(&mut stream, ClientMsg::Attach { id });

    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("project-folder") {
        assert!(
            Instant::now() < deadline,
            "pwd never printed the folder: {:?}",
            String::from_utf8_lossy(&output)
        );
        match recv(&mut stream) {
            ServerMsg::Output { data, .. } | ServerMsg::Attached { replay: data, .. } => {
                output.extend(data)
            }
            _ => {}
        }
    }

    send(&mut stream, ClientMsg::Shutdown);
    server.join().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn agents_refuse_to_start_without_a_project_folder() {
    let dir = std::env::temp_dir().join(format!("mux-it-agent-{}", std::process::id()));
    let socket = dir.join("mux.sock");
    let server = {
        let (socket, state) = (socket.clone(), dir.join("state.json"));
        std::thread::spawn(move || mux_server::run_at(socket, state))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("server never came up: {err}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let send = |stream: &mut UnixStream, msg| protocol::write(stream, &msg).unwrap();
    let recv = |stream: &mut UnixStream| protocol::read::<ServerMsg>(stream).unwrap();

    send(&mut stream, ClientMsg::Hello { protocol: PROTOCOL });
    send(
        &mut stream,
        ClientMsg::CreateGroup {
            name: "no folder".to_owned(),
            path: None,
        },
    );
    let group = loop {
        if let ServerMsg::State { groups } = recv(&mut stream)
            && let Some(group) = groups.iter().find(|g| g.name == "no folder")
        {
            break group.id;
        }
    };
    send(
        &mut stream,
        ClientMsg::CreateSession {
            group: Some(group),
            title: "New agent".to_owned(),
            kind: SessionKind::Agent,
        },
    );
    let id = loop {
        if let ServerMsg::Created { id } = recv(&mut stream) {
            break id;
        }
    };
    send(&mut stream, ClientMsg::AgentOpen { id });
    send(
        &mut stream,
        ClientMsg::AgentSend {
            id,
            text: "delete everything".to_owned(),
            attachments: Vec::new(),
        },
    );

    let error = loop {
        assert!(Instant::now() < deadline, "no error came back");
        if let ServerMsg::AgentItem {
            item: AgentItem::Error { message },
            ..
        } = recv(&mut stream)
        {
            break message;
        }
    };
    assert!(
        error.contains("no project folder"),
        "unexpected error: {error}"
    );

    send(&mut stream, ClientMsg::Shutdown);
    server.join().unwrap().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

// drives a real opencode turn through the server, so it needs opencode installed and signed in
#[test]
#[ignore]
fn opencode_live_turn() {
    use mux_core::protocol::{AgentSettings, Decision};
    use mux_core::{Access, Provider};

    let dir = std::env::temp_dir().join(format!("mux-it-opencode-{}", std::process::id()));
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("a.txt"), "hello\n").unwrap();
    // asks before running commands, so the approval round trip is exercised too
    std::fs::write(
        project.join("opencode.json"),
        r#"{"permission":{"bash":"ask","edit":"allow"}}"#,
    )
    .unwrap();
    let socket = dir.join("mux.sock");
    let _server = {
        let (socket, state) = (socket.clone(), dir.join("state.json"));
        std::thread::spawn(move || mux_server::run_at(socket, state))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("server never came up: {err}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let send = |stream: &mut UnixStream, msg| protocol::write(stream, &msg).unwrap();
    let recv = |stream: &mut UnixStream| protocol::read::<ServerMsg>(stream).unwrap();

    send(&mut stream, ClientMsg::Hello { protocol: PROTOCOL });
    send(
        &mut stream,
        ClientMsg::CreateGroup {
            name: "oc".to_owned(),
            path: Some(project.to_string_lossy().into_owned()),
        },
    );
    let group = loop {
        if let ServerMsg::State { groups } = recv(&mut stream)
            && let Some(group) = groups.iter().find(|g| g.name == "oc")
        {
            break group.id;
        }
    };
    send(
        &mut stream,
        ClientMsg::CreateSession {
            group: Some(group),
            title: "New agent".to_owned(),
            kind: SessionKind::Agent,
        },
    );
    let id = loop {
        if let ServerMsg::Created { id } = recv(&mut stream) {
            break id;
        }
    };
    send(&mut stream, ClientMsg::AgentOpen { id });
    let model = std::env::var("MUX_OPENCODE_MODEL").unwrap_or("opencode/big-pickle".to_owned());
    send(
        &mut stream,
        ClientMsg::AgentConfigure {
            id,
            settings: AgentSettings {
                provider: Provider::OpenCode,
                model: Some(model),
                access: Access::AutoEdits,
                ..AgentSettings::default()
            },
        },
    );
    send(
        &mut stream,
        ClientMsg::AgentSend {
            id,
            text: "Use the shell tool to run `echo mux-ok`. Then append the line `world` to a.txt with an edit. Reply in one short line.".to_owned(),
            attachments: Vec::new(),
        },
    );

    let deadline = Instant::now() + Duration::from_secs(240);
    let mut items: Vec<AgentItem> = Vec::new();
    let mut was_busy = false;
    loop {
        assert!(Instant::now() < deadline, "the turn never finished");
        match recv(&mut stream) {
            ServerMsg::AgentItem { index, item, .. } => {
                if let AgentItem::Approval {
                    id: approval,
                    decision: None,
                    title,
                    detail,
                } = &item
                {
                    eprintln!("approval: {title} {detail}");
                    send(
                        &mut stream,
                        ClientMsg::AgentDecide {
                            id,
                            approval: *approval,
                            decision: Decision::Allow,
                        },
                    );
                }
                if index >= items.len() {
                    items.resize(
                        index + 1,
                        AgentItem::Error {
                            message: String::new(),
                        },
                    );
                }
                items[index] = item;
            }
            ServerMsg::AgentBusy { busy: true, .. } => was_busy = true,
            ServerMsg::AgentBusy { busy: false, .. } if was_busy => break,
            _ => {}
        }
    }
    for item in &items {
        eprintln!("{item:?}");
    }
    assert!(items.iter().any(|i| matches!(
        i,
        AgentItem::Approval {
            decision: Some(Decision::Allow),
            ..
        }
    )));
    assert!(
        items
            .iter()
            .any(|i| matches!(i, AgentItem::Command { output, .. } if output.contains("mux-ok")))
    );
    assert!(
        items
            .iter()
            .any(|i| matches!(i, AgentItem::FileChange { changes, .. } if !changes.is_empty()))
    );
    assert!(
        items
            .iter()
            .any(|i| matches!(i, AgentItem::Assistant { text } if !text.is_empty()))
    );
    assert!(
        std::fs::read_to_string(project.join("a.txt"))
            .unwrap()
            .contains("world")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
