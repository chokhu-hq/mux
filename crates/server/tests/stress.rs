// the server under load: many agents at once, clients that vanish mid-turn or stop reading, and big replies
mod common;

use common::Server;
use mux_core::protocol::{AgentItem, AgentSettings, ClientMsg, ItemStatus};

#[cfg(target_os = "linux")]
fn resident_mb(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap();
    kb / 1024
}

#[cfg(target_os = "linux")]
fn open_files(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .count()
}

#[cfg(target_os = "linux")]
// processes the server started, from any of its threads
fn children(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|task| {
            std::fs::read_to_string(task.path().join("children"))
                .unwrap_or_default()
                .split_whitespace()
                .count()
        })
        .sum()
}

fn replies(server: &Server, id: u64) -> usize {
    server.agents.get(&id).map_or(0, |(items, _)| {
        items
            .iter()
            .filter(|i| matches!(i, AgentItem::Assistant { .. }))
            .count()
    })
}

fn all_replied(server: &Server, ids: &[u64], count: usize) -> bool {
    ids.iter().all(|id| {
        server.agents.get(id).is_some_and(|(_, busy)| !busy) && replies(server, *id) >= count
    })
}

#[cfg(target_os = "linux")]
#[test]
fn a_client_that_stops_reading_cant_grow_the_server() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "hello");
    server.wait_idle();
    // a window that froze, still connected and watching but never reading
    let mut frozen = server.client();
    mux_core::protocol::write(&mut frozen, &ClientMsg::AgentOpen { id }).unwrap();
    let before = resident_mb(server.pid());
    for round in 1..=3 {
        server.say(id, "scenario:flood");
        server.wait("the flood to finish", |s| {
            !s.busy && s.assistant_text().len() > round
        });
    }
    let grown = resident_mb(server.pid()).saturating_sub(before);
    assert!(
        grown < 48,
        "the server grew by {grown} MB holding output for a client that reads nothing"
    );
    drop(frozen);
}

#[cfg(target_os = "linux")]
#[test]
fn many_agents_and_vanishing_clients_leave_nothing_behind() {
    // tight enough that leaked sockets or pipes would run the server out
    let mut server = Server::start_limited(&[], Some(256));
    let ids: Vec<u64> = (0..16)
        .map(|_| server.claude_agent(AgentSettings::default()))
        .collect();
    for id in &ids {
        server.say(*id, "hello");
    }
    server.wait("every agent to answer", |s| all_replied(s, &ids, 1));
    let files = open_files(server.pid());
    assert_eq!(children(server.pid()), 16, "one claude per agent");

    for round in 0..3 {
        // other windows watch every agent, then disappear in the middle of the turns
        let mut watchers: Vec<_> = (0..6).map(|_| server.client()).collect();
        for watcher in &mut watchers {
            for id in &ids {
                mux_core::protocol::write(watcher, &ClientMsg::AgentOpen { id: *id }).unwrap();
            }
        }
        for id in &ids {
            server.say(*id, "scenario:tool");
        }
        server.wait("the commands to start", |s| {
            ids.iter().all(|id| {
                s.agents.get(id).is_some_and(|(items, _)| {
                    items.iter().any(|i| {
                        matches!(
                            i,
                            AgentItem::Command {
                                status: ItemStatus::Running,
                                ..
                            }
                        )
                    })
                })
            })
        });
        drop(watchers);
        server.wait("every agent to finish", |s| all_replied(s, &ids, round + 2));
    }
    // the vanished clients' sockets are closed again, give or take the model listings' pipes
    let after = open_files(server.pid());
    assert!(
        after <= files + 4,
        "{files} files open before, {after} after"
    );
    assert_eq!(children(server.pid()), 16, "no extra processes");

    for id in &ids {
        server.send(ClientMsg::DeleteSession { id: *id });
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while children(server.pid()) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        children(server.pid()),
        0,
        "deleting the agents stopped their processes"
    );
}
