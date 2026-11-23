//! 补清复核系统身份。定时脚本还须核验其登记进程组。

use std::{io, time::Duration};

use serde::{Deserialize, Serialize};

const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const EXIT_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    pub(super) pid: u32,
    user: String,
    created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot: Option<String>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) use platform::verify_group_exit;
pub(super) use platform::{capture, terminate_if_matching};

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn owned_parent_exited(pid: u32) -> io::Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(invalid_pid)?;
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())
    .map_err(io::Error::from)
}

fn invalid_pid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid background process ID")
}

fn exit_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "background PID exit not confirmed")
}

pub(super) fn process_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || {
        #[cfg(unix)]
        {
            error.raw_os_error() == Some(3)
        } // ESRCH
        #[cfg(not(unix))]
        {
            false
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn group_already_exited(pid: u32) -> io::Result<bool> {
    use libproc::processes::{ProcFilter, pids_by_type};
    // Darwin 的 killpg 对只剩僵尸的进程组可返回 EPERM。先确认主子进程已退出，
    // 再核对组中已无活进程；真正的权限失败仍保留清理依据。
    if !owned_parent_exited(pid)? {
        return Ok(false);
    }
    nix::errno::Errno::clear();
    let pids = pids_by_type(ProcFilter::ByProgramGroup { pgrpid: pid })?;
    Ok(pids.iter().all(|member| *member == pid))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{thread, time::Instant};

    use libproc::{bsd_info::BSDInfo, proc_pid::pidinfo};
    use nix::{
        errno::Errno,
        sys::event::{EventFilter, EventFlag, FilterFlag, KEvent, Kqueue},
        sys::signal::{Signal, kill, killpg},
        unistd::{Pid, geteuid, getpgid},
    };

    use super::*;

    fn probe(pid: u32) -> io::Result<Option<(Identity, bool)>> {
        let raw = i32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(invalid_pid)?;
        match pidinfo::<BSDInfo>(raw, 0) {
            Ok(info) if info.pbi_pid == pid && info.pbi_start_tvsec != 0 => {
                let created = info
                    .pbi_start_tvsec
                    .checked_mul(1_000_000)
                    .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
                    .ok_or_else(|| io::Error::other("invalid Darwin process creation time"))?;
                Ok(Some((
                    Identity {
                        pid,
                        user: info.pbi_uid.to_string(),
                        created,
                        boot: None,
                    },
                    info.pbi_status != 5,
                ))) // SZOMB
            }
            Err(_) if exit_observed(pid).unwrap_or(false) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
            Ok(_) => Err(io::Error::other("incomplete Darwin process identity")),
        }
    }

    pub(in super::super) fn capture(pid: u32) -> io::Result<Option<Identity>> {
        Ok(probe(pid)?.map(|(identity, _)| identity))
    }

    fn exit_observed(pid: u32) -> nix::Result<bool> {
        // proc_pidinfo 不读取僵尸进程，kill(pid, 0) 仍可成功。用内核退出通知区分，
        // 查询权限或资源失败时保留登记。
        let queue = Kqueue::new()?;
        let registration = KEvent::new(
            pid as usize,
            EventFilter::EVFILT_PROC,
            EventFlag::EV_ADD | EventFlag::EV_ONESHOT,
            FilterFlag::NOTE_EXIT,
            0,
            0,
        );
        let now = nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        match queue.kevent(&[registration], &mut [], Some(now)) {
            Ok(_) => queue
                .kevent(&[], &mut [registration], Some(now))
                .map(|count| count > 0),
            Err(Errno::ESRCH) => Ok(true),
            Err(error) => Err(error),
        }
    }

    pub(in super::super) fn terminate_if_matching(
        expected: &Identity,
        managed_group: bool,
    ) -> io::Result<()> {
        if expected.user != geteuid().as_raw().to_string() {
            return Ok(());
        }
        let matches = || -> io::Result<bool> {
            Ok(probe(expected.pid)?.is_some_and(|(actual, alive)| alive && actual == *expected))
        };
        // Darwin 没有用于信号的 pidfd。紧邻发送信号重查 UID 与微秒级创建时间。
        if !matches()? {
            return Ok(());
        }
        let pid = Pid::from_raw(i32::try_from(expected.pid).map_err(|_| invalid_pid())?);
        let termination = if managed_group {
            if getpgid(Some(pid))? != pid {
                return Err(io::Error::other(
                    "scheduled shell is no longer its registered group leader",
                ));
            }
            killpg(pid, Signal::SIGKILL)
        } else {
            kill(pid, Signal::SIGKILL)
        };
        match termination {
            Err(Errno::ESRCH) => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(()) => {}
        }
        let deadline = Instant::now() + EXIT_TIMEOUT;
        while matches()? {
            if Instant::now() >= deadline {
                return Err(exit_timeout());
            }
            thread::sleep(EXIT_POLL);
        }
        Ok(())
    }

    pub(in super::super) fn verify_group_exit(pid: u32) -> io::Result<()> {
        use libproc::processes::{ProcFilter, pids_by_type};
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            nix::errno::Errno::clear();
            let mut live = false;
            for member in pids_by_type(ProcFilter::ByProgramGroup { pgrpid: pid })? {
                live |= probe(member)?.is_some_and(|(_, alive)| alive);
            }
            if !live {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(exit_timeout());
            }
            thread::sleep(EXIT_POLL);
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::Errno,
        process::{
            Pid, PidfdFlags, Signal, geteuid, getpgid, kill_process_group, pidfd_open,
            pidfd_send_signal, test_kill_process_group,
        },
    };
    use std::{fs, time::Instant};

    fn stat(pid: u32) -> io::Result<(bool, u64)> {
        // comm 可含任意字节和右括号。解析分隔符后的内核数字字段，不以名称判断身份。
        let stat = fs::read(format!("/proc/{pid}/stat"))?;
        let fields = stat
            .windows(2)
            .rposition(|window| window == b") ")
            .and_then(|boundary| std::str::from_utf8(&stat[boundary + 2..]).ok())
            .ok_or_else(|| io::Error::other("invalid proc stat"))?;
        let created = fields
            .split_whitespace()
            .nth(19)
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| io::Error::other("missing proc creation time"))?;
        Ok((
            !fields.starts_with("Z ") && !fields.starts_with("X "),
            created,
        ))
    }

    fn probe(pid: u32) -> io::Result<Option<(Identity, bool)>> {
        let read = || -> io::Result<_> {
            let (alive, created) = stat(pid)?;
            let status = fs::read(format!("/proc/{pid}/status"))?;
            let user = status
                .split(|byte| *byte == b'\n')
                .find_map(|line| line.strip_prefix(b"Uid:"))
                .and_then(|line| std::str::from_utf8(line).ok())
                .and_then(|uids| uids.split_whitespace().nth(1))
                .ok_or_else(|| io::Error::other("missing effective UID"))?;
            let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .map_err(|error| io::Error::other(format!("read Linux boot identity: {error}")))?;
            Ok((
                Identity {
                    pid,
                    user: user.into(),
                    created,
                    boot: Some(boot.trim().into()),
                },
                alive,
            ))
        };
        match read() {
            Ok(identity) => Ok(Some(identity)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(in super::super) fn capture(pid: u32) -> io::Result<Option<Identity>> {
        Ok(probe(pid)?.map(|(identity, _)| identity))
    }

    pub(in super::super) fn terminate_if_matching(
        expected: &Identity,
        managed_group: bool,
    ) -> io::Result<()> {
        if expected.user != geteuid().as_raw().to_string() {
            return Ok(());
        }
        let pid = i32::try_from(expected.pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(invalid_pid)?;
        // 已固定的句柄只会终止同一个对象。无 pidfd 时保留记录，避免降级为按数字发送信号。
        let fd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(Errno::SRCH) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !probe(expected.pid)?.is_some_and(|(actual, alive)| alive && actual == *expected) {
            return Ok(());
        }
        let termination = if managed_group {
            if getpgid(Some(pid))? != pid {
                return Err(io::Error::other(
                    "scheduled shell is no longer its registered group leader",
                ));
            }
            kill_process_group(pid, Signal::KILL)
        } else {
            pidfd_send_signal(&fd, Signal::KILL)
        };
        match termination {
            Err(Errno::SRCH) => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(()) => {}
        }
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let wait = Timespec {
            tv_sec: 0,
            tv_nsec: EXIT_POLL.subsec_nanos().into(),
        };
        loop {
            match poll(&mut fds, Some(&wait)) {
                Ok(_) if fds[0].revents().intersects(PollFlags::IN | PollFlags::HUP) => {
                    return Ok(());
                }
                Ok(_) | Err(Errno::INTR) if Instant::now() < deadline => {}
                Ok(_) | Err(Errno::INTR) => return Err(exit_timeout()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(in super::super) fn verify_group_exit(pid: u32) -> io::Result<()> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(invalid_pid)?;
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            match test_kill_process_group(pid) {
                Err(Errno::SRCH) => return Ok(()),
                Err(error) => return Err(error.into()),
                Ok(()) if Instant::now() >= deadline => return Err(exit_timeout()),
                Ok(()) => std::thread::sleep(EXIT_POLL),
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use winsafe::{HPROCESS, TokenInfo, co, guard::CloseHandleGuard};

    use super::*;

    const QUERY: co::PROCESS = co::PROCESS::QUERY_LIMITED_INFORMATION;

    fn os_error(error: co::ERROR) -> io::Error {
        io::Error::from_raw_os_error(error.raw() as i32)
    }

    fn open(pid: u32, flags: co::PROCESS) -> io::Result<Option<CloseHandleGuard<HPROCESS>>> {
        if pid == 0 {
            return Err(invalid_pid());
        }
        match HPROCESS::OpenProcess(flags | co::PROCESS::SYNCHRONIZE, false, pid) {
            Ok(handle) => Ok(Some(handle)),
            Err(co::ERROR::INVALID_PARAMETER) => Ok(None),
            Err(error) => Err(os_error(error)),
        }
    }

    fn owner(handle: &HPROCESS) -> io::Result<String> {
        let token = handle
            .OpenProcessToken(co::TOKEN::QUERY)
            .map_err(os_error)?;
        let info = token
            .GetTokenInformation(co::TOKEN_INFORMATION_CLASS::User)
            .map_err(os_error)?;
        let TokenInfo::User(user) = info else {
            return Err(io::Error::other("invalid background process token"));
        };
        user.User
            .Sid()
            .as_deref()
            .map(ToString::to_string)
            .ok_or_else(|| io::Error::other("missing process SID"))
    }

    fn identify(pid: u32, handle: &HPROCESS) -> io::Result<Identity> {
        let (created, _, _, _) = handle.GetProcessTimes().map_err(os_error)?;
        Ok(Identity {
            pid,
            user: owner(handle)?,
            created: created.into(),
            boot: None,
        })
    }

    pub(in super::super) fn capture(pid: u32) -> io::Result<Option<Identity>> {
        let Some(handle) = open(pid, QUERY)? else {
            return Ok(None);
        };
        match identify(pid, &handle) {
            Ok(identity) => Ok(Some(identity)),
            Err(_) if handle.WaitForSingleObject(Some(0)) == Ok(co::WAIT::OBJECT_0) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(in super::super) fn terminate_if_matching(
        expected: &Identity,
        _managed_group: bool,
    ) -> io::Result<()> {
        if expected.user != owner(&HPROCESS::GetCurrentProcess())? {
            return Ok(());
        }
        let Some(handle) = open(expected.pid, QUERY)? else {
            return Ok(());
        };
        if handle.WaitForSingleObject(Some(0)).map_err(os_error)? == co::WAIT::OBJECT_0
            || identify(expected.pid, &handle)? != *expected
        {
            return Ok(());
        }
        // Query 句柄仍固定原对象，带终止权限的句柄再次验证 SID 和 FILETIME。
        let Some(target) = open(expected.pid, QUERY | co::PROCESS::TERMINATE)? else {
            return Ok(());
        };
        if target.WaitForSingleObject(Some(0)).map_err(os_error)? == co::WAIT::OBJECT_0
            || identify(expected.pid, &target)? != *expected
        {
            return Ok(());
        }
        if let Err(error) = target.TerminateProcess(1)
            && target.WaitForSingleObject(Some(0)).map_err(os_error)? != co::WAIT::OBJECT_0
        {
            return Err(os_error(error));
        }
        match target
            .WaitForSingleObject(Some(EXIT_TIMEOUT.as_millis() as u32))
            .map_err(os_error)?
        {
            co::WAIT::OBJECT_0 => Ok(()),
            _ => Err(exit_timeout()),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::*;

    pub(in super::super) fn capture(_: u32) -> io::Result<Option<Identity>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "background shell process identity",
        ))
    }

    pub(in super::super) fn terminate_if_matching(
        _: &Identity,
        _managed_group: bool,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "background shell process recovery",
        ))
    }
}
