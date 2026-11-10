# 受控第三方依赖

## Tauri Updater 2.12.0

根 `Cargo.toml` 通过 `[patch.crates-io]` 使用 `tauri-plugin-updater/`，排除在产品 workspace 成员之外。
该目录保留完整发布包与上游许可证，禁止改 registry 缓存。

- 来源：crates.io `tauri-plugin-updater` 2.12.0 发布包，原始提交见 `.cargo_vcs_info.json`。
- 发布包 SHA-256：`7a5cad8ed5948d988e1018ecd31e27cacedbf72ce3fb972940c3e4bf51639e4b`。
- 许可证：`tauri-plugin-updater/LICENSE_APACHE-2.0` 或 `tauri-plugin-updater/LICENSE_MIT`。
- 唯一代码改动：`src/updater.rs` 提取原有 Update 构造逻辑，增加同步 `update_from_response` 入口。

客户端业务层需要先读取有界响应、验签并以已配置中心补全下载路径。上游 `check()` 将请求和
绝对 URL 解析合在一起，`Update` 的安装上下文又不允许外部构造，因此无法直接传入已处理的响应。
补丁只开放此接入点；不改变网络 `check()`、版本比较、平台选择、验签或各平台安装行为。
产品更新发现不再调用 SDK 的网络检查，防止重复请求和两份响应不一致。

回归入口：`cargo test -p ez-assistant-desktop --lib desktop_update::tests`，覆盖原始签名响应、
相对路径及旧绝对 URL 的解析、SDK 候选创建和原有下载／取消校验。测试不实际安装更新。
待上游提供可接入预处理响应的等效 API 后，先通过该回归，再迁移调用并删除此覆盖；由 Cargo 更新锁文件。

## Wry 0.55.1

根 `Cargo.toml` 通过 `[patch.crates-io]` 使用 `wry/`；该目录排除在产品 workspace 成员之外，
保留上游独立许可证与编程风格。正式构建、原生验证和跨平台构建都必须使用同一覆盖，不能修改
Cargo registry 缓存或只在本机构建前临时打补丁。

- 来源：[crates.io Wry 0.55.1](https://crates.io/crates/wry/0.55.1)，完整发布包。
- 发布包 SHA-256：`186f9871daa55fd9c016578b810d149de58367113db7fb72b462d2323ce19514`。
- 许可证：`wry/LICENSE-APACHE` 或 `wry/LICENSE-MIT`；原始提交见 `wry/.cargo_vcs_info.json`。
- 本地改动仅为 `wry/src/webview2/mod.rs` 的初始化循环与 `wry/src/lib.rs` 的对应平台说明。

Windows WebView2 的 document-created 注入会运行于子 frame；原始 Wry 忽略
`for_main_frame_only`，导致 Tauri 核心与 IPC 调用凭据进入不可信页面。补丁在这些脚本执行前
检查 `window.parent === window`，显式 all-frame 脚本保持原样。macOS／Linux 实现未改。
main-only 脚本因此具有块作用域，跨初始化脚本共享状态须使用 `window` 属性或 IIFE；
现有 Tauri 核心、主 frame IPC、子 frame 隔离与 all-frame 标记均有原生验证。

回归入口为 `apps/desktop` 的 `npm run test:native:mcp-apps`，先构建正式 Host 和带
`tauri/custom-protocol` 的 `mcp_apps_probe`。原生探针必须报告唯一 PASS、没有 FAIL 且退出码为 0，
不能只依赖退出码。该检查覆盖真实 Host 资源／静态代理、标准握手、主 frame IPC、子 frame
DOM／Cookie／Tauri 隔离和卸载。

2026-11-05 已核对上游 `wry-v0.57.1` 与 `dev`，相同循环仍未落实该标记。待兼容上游修复可用时，
先在 Windows 通过上述原生隔离及主／子 frame 标记回归，再删除根覆盖与整个 `wry/`；
由 Cargo 更新锁文件，不能仅删除本地条件或跳过原生验证。
