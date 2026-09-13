//! Runtime Host 私有平台能力边界。
//!
//! 平台专属实现只存在于 [`unix`] 与 [`windows`] 子模块；endpoint、config_source、
//! storage、image 等业务适配统一通过这里导出的原语访问私有文件与进程脱离，
//! 不得在各业务模块内直接使用 `std::os::*` 专属 API。
//!
//! Windows 安全模型（v0.25.3 技术方案 3.1）："owner 校验 + 用户 Profile ACL 继承 +
//! 一律拒绝 reparse point"，不逐项移植 Unix 的 mode/dev-ino 检查；目录 fsync 无
//! Windows 等价物，原子替换降级为 rename 后重读校验。Windows 原生安全 FFI
//! 按 2026-09-12 用户批准的局部例外封装，业务模块不接触裸指针和原生资源生命周期。

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;
