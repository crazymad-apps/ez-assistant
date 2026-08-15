//! Runtime Host 私有平台能力边界。

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::launch_detached;
