//! ConPTY 子树的唯一 Job owner；句柄由 OwnedHandle 持有，可在线程间安全移动。

use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    },
    Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
};

pub(crate) struct Job(OwnedHandle);

impl Job {
    #[allow(unsafe_code)]
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: 无名称、无继承的私有 Job；成功返回的独占句柄立即交给 OwnedHandle。
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: job 有效且独占；传入正确对齐的完整结构，系统不保留其指针。
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    #[allow(unsafe_code)]
    pub(crate) fn assign(&self, pid: u32) -> io::Result<()> {
        // SAFETY: pid 来自刚创建且尚未 wait 的子进程；只申请 Job 分配所需权限，不继承句柄。
        let raw = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: OpenProcess 的独占返回句柄只关闭一次，不导出到调用方。
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process.as_raw_handle()) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    pub(crate) fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: 借用 Job 持续有效；输出结构大小和对齐正确，不跨调用保留指针。
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(info.ActiveProcesses)
    }

    /// 显式关闭路径先终止并核验；Drop 的 kill-on-close 只作为异常退出兜底。
    #[allow(unsafe_code)]
    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: 当前 owner 的有效 Job 句柄；终止仅影响分配给本 Job 的终端子树。
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.active_processes()? != 0 {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "terminal job did not become empty",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
