//! 首次插件初始化串行化，补齐上游 mutex 已存在而隐藏接收窗口尚未创建的窗口期。
use std::{
    marker::PhantomData,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    rc::Rc,
};
use windows_sys::Win32::{
    Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0},
    System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
};

pub(crate) struct StartupGuard {
    handle: OwnedHandle,
    // Windows mutex 必须由获取它的线程释放；不能将该 guard 移入异步任务。
    _thread: PhantomData<Rc<()>>,
}

impl StartupGuard {
    #[allow(unsafe_code)]
    pub(crate) fn acquire(identifier: &str) -> std::io::Result<Self> {
        let name: Vec<u16> = format!("Local\\{identifier}-desktop-startup")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: 名称在调用期间为带 NUL 的有效 UTF-16；默认安全描述符，不传裸用户指针。
        let raw = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateMutexW 返回的独占句柄转交 OwnedHandle，任何返回路径都会关闭它。
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: 有效的 mutex 句柄，同步有界等待；abandoned 也代表本线程已取得所有权。
        let wait = unsafe { WaitForSingleObject(handle.as_raw_handle(), 30_000) };
        if !matches!(wait, WAIT_OBJECT_0 | WAIT_ABANDONED) {
            return Err(std::io::Error::other("desktop startup mutex unavailable"));
        }
        Ok(Self {
            handle,
            _thread: PhantomData,
        })
    }
}

impl Drop for StartupGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: !Send guard 保证仍在获取 mutex 的线程；OwnedHandle 随后只关闭一次。
        unsafe {
            ReleaseMutex(self.handle.as_raw_handle());
        }
    }
}
