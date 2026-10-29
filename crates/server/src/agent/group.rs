// an agent cli starts helpers of its own, like mcp servers and shells, which outlive it if only the cli is
// killed, and nothing at all is killed when the server crashes. so everything an agent starts is kept together
// and stopped together once the server lets go of it. the server lets go when the agent is done with, and the
// system lets go for it when the server dies however it dies.
// on unix that's a process group with a small watcher inside it, and on windows a job that kills what's in it
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(unix)]
use std::process::Stdio;

#[cfg(unix)]
// ignores the stop it sends the group, so it can follow up on anything that ignored it too.
// while it runs the group's id can't be handed to another group, so the kill never lands anywhere else
const WATCHER: &str = "trap '' TERM HUP INT; read _ <&3; kill -TERM 0; sleep 3; kill -KILL 0";

// held by the server for as long as the agent's group should live
pub(crate) struct Group {
    #[cfg(unix)]
    _pipe: OwnedFd,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

// puts the agent in a job that ends everything in it once its last handle closes. what the agent starts joins it
#[cfg(windows)]
pub(crate) fn watch(agent: &tokio::process::Child) -> io::Result<Group> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    let process = agent
        .raw_handle()
        .ok_or_else(|| io::Error::other("the agent already exited"))?;
    // the job is a new handle owned here, and the limits are read during the call
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job_handle = OwnedHandle::from_raw_handle(job as _);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        if AssignProcessToJobObject(job, process as _) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Group { _job: job_handle })
    }
}

// puts a watcher into the group of the agent, which was started with a process group of its own
#[cfg(unix)]
pub(crate) fn watch(agent: &tokio::process::Child) -> io::Result<Group> {
    let leader = agent
        .id()
        .ok_or_else(|| io::Error::other("the agent already exited"))?;
    // both ends are closed on exec, so no other process the server starts can hold the pipe open
    let (reader, writer) = io::pipe()?;
    let reader = OwnedFd::from(reader);
    let raw = reader.as_raw_fd();
    let group = leader as libc::pid_t;
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args(["-c", WATCHER])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // runs in the watcher between fork and exec, and only makes system calls
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, group) != 0 {
                return Err(io::Error::last_os_error());
            }
            // fd 3 is the read end, and the copy dup2 makes is kept across exec
            if raw == 3 {
                libc::fcntl(3, libc::F_SETFD, 0);
            } else if libc::dup2(raw, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut watcher = command.spawn()?;
    drop(reader);
    // reaped once it's done, so it doesn't linger as a zombie
    tokio::spawn(async move {
        let _ = watcher.wait().await;
    });
    Ok(Group {
        _pipe: OwnedFd::from(writer),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn alive(pid: i32) -> bool {
        // signal 0 only checks, and fails once the process is gone and reaped
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[tokio::test]
    async fn letting_go_stops_the_agent_and_what_it_started() {
        let mut agent = tokio::process::Command::new("/bin/sh");
        agent
            .args(["-c", "sleep 300 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut agent = agent.spawn().unwrap();
        let group = watch(&agent).unwrap();
        let mut out = tokio::io::BufReader::new(agent.stdout.take().unwrap());
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut out, &mut line)
            .await
            .unwrap();
        let helper: i32 = line.trim().parse().unwrap();
        assert!(alive(helper));
        drop(group);
        let _ = agent.wait().await;
        for _ in 0..100 {
            if !alive(helper) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the agent's helper outlived it");
    }
}
