//! Native process-table primitives. The watcher retains its unreaped direct
//! child, reserving the original process-group ID through confirmed cleanup.
use std::io;

pub(super) fn signal(group: i32, signal: i32) {
    // SAFETY: group is our own unreaped direct child, never an unbound PID.
    unsafe {
        libc::killpg(group, signal);
    }
}
pub(super) fn exited(pid: i32) -> io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: info is sized for waitid; WNOWAIT preserves the child PID and
    // WNOHANG never waits for an active process.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            u32::try_from(pid).unwrap(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    #[cfg(target_os = "linux")]
    return Ok(unsafe { info.si_pid() } == pid);
    #[cfg(target_os = "macos")]
    Ok(info.si_pid == pid)
}

#[cfg(target_os = "linux")]
pub(super) fn quiet(group: i32) -> io::Result<bool> {
    let mut examined = 0;
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        examined += 1;
        if examined > 65536 {
            return Err(io::Error::other(
                "Process cleanup inventory exceeds its limit",
            ));
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let (_, fields) = stat
            .rsplit_once(')')
            .ok_or_else(|| io::Error::other("Invalid process identity"))?;
        let fields = fields.split_whitespace().take(3).collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(io::Error::other("Incomplete process identity"));
        }
        let pgid = fields[2].parse::<i32>().map_err(io::Error::other)?;
        if pgid == group {
            if fields[0] != "Z" && fields[0] != "X" {
                return Ok(false);
            }
            // A dead group leader can still have a thread blocked in the
            // kernel. Its zombie state does not prove complete cleanup.
            let tasks = match std::fs::read_dir(entry.path().join("task")) {
                Ok(tasks) => tasks,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for task in tasks {
                examined += 1;
                if examined > 65536 {
                    return Err(io::Error::other(
                        "Process cleanup inventory exceeds its limit",
                    ));
                }
                let stat = match std::fs::read_to_string(task?.path().join("stat")) {
                    Ok(stat) => stat,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                let (_, fields) = stat
                    .rsplit_once(')')
                    .ok_or_else(|| io::Error::other("Invalid thread identity"))?;
                let state = fields
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| io::Error::other("Incomplete thread identity"))?;
                if state != "Z" && state != "X" {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}
#[cfg(target_os = "macos")]
pub(super) fn quiet(group: i32) -> io::Result<bool> {
    let mut pids = vec![0i32; 4096];
    let bytes = i32::try_from(pids.len() * std::mem::size_of::<i32>()).unwrap();
    // SAFETY: proc_listpids writes at most the supplied buffer length. 2 is
    // PROC_PGRP_ONLY from libproc's public header.
    // SAFETY: errno is thread-local. Distinguish an empty successful list from
    // an API failure which also returns zero; this watcher has one thread.
    unsafe {
        *libc::__error() = 0;
    }
    let filled = unsafe {
        libc::proc_listpids(
            2,
            u32::try_from(group).unwrap(),
            pids.as_mut_ptr().cast(),
            bytes,
        )
    };
    if filled < 0 {
        return Err(io::Error::last_os_error());
    }
    if filled == 0 {
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code != 0 && code != libc::ESRCH)
        {
            return Err(error);
        }
    }
    if filled >= bytes {
        return Err(io::Error::other(
            "Process group inventory exceeds its limit",
        ));
    }
    for pid in pids
        .into_iter()
        .take(usize::try_from(filled).unwrap() / std::mem::size_of::<i32>())
        .filter(|pid| *pid > 0)
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let expected = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).unwrap();
        // SAFETY: info is sized for PROC_PIDTBSDINFO and read only on a full reply.
        let received = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                expected,
            )
        };
        if received == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            continue;
        }
        if received != expected {
            return Err(io::Error::other("Process group cleanup is unconfirmed"));
        }
        let info = unsafe { info.assume_init() };
        if info.pbi_pgid == u32::try_from(group).unwrap() && info.pbi_status != libc::SZOMB {
            return Ok(false);
        }
    }
    Ok(true)
}
