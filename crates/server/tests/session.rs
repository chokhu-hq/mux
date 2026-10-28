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
