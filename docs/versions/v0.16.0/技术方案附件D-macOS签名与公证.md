# v0.16.0 技术方案附件 D：macOS Developer ID 签名与公证

## 一、附件信息

- 状态：已确认（2026-08-18）
- 主方案：[`v0.16.0 技术方案`](技术方案.md)
- 功能基线：[`v0.16.0 功能设计`](功能设计.md)
- 目标平台：macOS arm64，Developer ID 站外分发

本文把现有可构建的 Tauri `.app`/`.dmg` 提升为可验证的 Developer ID 签名、公证和 stapled
分发产物。Mac App Store、Universal Binary、CI/CD、自动更新和其他平台签名不在本版本。
实际发布操作以 [`macOS 发布流程`](../../release/macOS发布流程.md) 为唯一手册。

## 二、现状与目标

当前仓库已具备：

- `apps/desktop/src-tauri/tauri.conf.json` 保存通用 bundle 基线，`apps/desktop/scripts/run-tauri.mjs`
  在 release build 时叠加 `tauri.macos.conf.json` 并注入 `externalBin`，随包放入
  `ez-assistant-runtime`；
- `apps/desktop/scripts/prepare-sidecar.mjs` 按架构准备 Runtime sidecar；
- `apps/desktop/scripts/run-tauri.mjs` 在 release build 前准备 sidecar 并叠加 bundle 配置；
- v0.14.2 已生成 arm64 App/DMG，但产物为 ad-hoc 签名，严格 `codesign`/Gatekeeper 不能作为正式
  站外分发证据。

v0.16.0 目标产物必须同时满足：

1. 随包 Runtime 和 App 中所有可执行代码由同一个有效 `Developer ID Application` 身份签名；
2. App 使用 Hardened Runtime、安全时间戳和最小 entitlements；
3. DMG 中 App 布局完整，产物提交 Apple 公证成功；
4. App/DMG stapled ticket 可离线验证，Gatekeeper 接受从 DMG 安装的 App；
5. 构建和验证过程不把私钥、API key 或 app-specific password 写入仓库/日志。

## 三、官方链路与工具选择

使用 Tauri 2 官方 macOS 签名/公证集成作为构建实现，使用 Apple 命令行工具作为独立验证：

- 签名身份通过 Tauri `bundle.macOS.signingIdentity` 或 `APPLE_SIGNING_IDENTITY` 提供；本方案优先环境
  变量，避免把具体证书名称固化在共享配置。
- 公证推荐 App Store Connect API Key：`APPLE_API_ISSUER`、`APPLE_API_KEY`、
  `APPLE_API_KEY_PATH`；本地备选为 `APPLE_ID`、app-specific `APPLE_PASSWORD`、`APPLE_TEAM_ID`。
- Tauri 构建负责按正确嵌套顺序签 Runtime、App 与 DMG，并完成 App 公证/staple；
  release 脚本在最终 DMG 生成后将其作为独立产物再提交一次公证并 staple，不写一套并行手工签名器。
- 独立验证使用 `codesign`、`spctl`、`xcrun stapler`、`hdiutil` 和 `xcrun notarytool log`。
- 禁止使用已废弃的 `altool`；禁止以 `codesign --deep --sign` 作为修复嵌套签名的方式。

官方依据：

- [Tauri 2 macOS code signing](https://v2.tauri.app/distribute/sign/macos/)
- [Apple：Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
- [Apple：Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [Apple：Create Developer ID certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)

## 四、证书与凭据准备

### 4.1 Developer ID 证书

在执行仓库改动或正式构建前做只读核验：

```bash
security find-identity -v -p codesigning
```

必须存在目标团队的 `Developer ID Application` identity，并记录证书 Common Name、Team ID、有效期和
SHA-1 identity hash。证书私钥必须在当前构建用户 Keychain 中可访问。`Apple Development`、
`Apple Distribution` 或只有证书没有私钥都不能替代。

不把 `.p12`、私钥或导出密码放进仓库。若未来迁移到 CI，导入临时 Keychain 属于后续版本方案。

### 4.2 公证凭据

推荐在 Apple Developer/App Store Connect 创建权限最小的 API Key，把 `.p8` 保存在用户私有目录，
权限限制为仅当前用户可读。运行前只把路径和 ID 放入当前 shell 环境。

可先用 Apple 工具验证凭据和 Team 归属，不提交产品产物。任何验证失败都在构建前停止，不回退到把
密码写入脚本。Apple ID 方案必须使用 app-specific password，不能使用 Apple 账号主密码。

### 4.3 日志脱敏

release 脚本只检查所需环境变量是否存在，不回显值。错误输出过滤潜在 token 和用户私有 `.p8`
绝对路径；允许输出 signing identity 的公开名称、Team ID、notary submission ID 和产物路径以便审计。

## 五、Tauri 配置

### 5.1 配置分层

继续保留当前配置职责：

- `tauri.conf.json`：应用通用配置与 bundle 基线；
- `run-tauri.mjs`：release build 前准备 sidecar，并把 external Runtime 配置叠加到本次构建；
- `tauri.macos.conf.json`：macOS 窗口、图标、Hardened Runtime、entitlements 和平台配置；
- 新增 `src-tauri/Entitlements.plist`：最小生产 entitlements；
- identity 和公证凭据：只从构建环境注入。

不要把本机完整证书名称、Team 密钥路径或 Apple ID 写进上述 JSON。若 Tauri 必须在配置中指定
entitlements 文件，只提交相对路径。

### 5.2 Hardened Runtime 与 entitlements

启用 Hardened Runtime 和 secure timestamp。Entitlements 以“没有就不加”为原则：

- 不加入 `com.apple.security.get-task-allow`；
- 不为方便调试加入 disable library validation、allow unsigned executable memory、JIT 等宽权限；
- Tauri/WebView 和随包 Rust Runtime 若无需额外 entitlement，则文件保持最小；
- 若实际构建或运行证明确需某项，必须记录触发功能、Apple 文档依据、安全影响和独立验证后再加入。

Runtime Host 访问用户文件、网络和启动工具依赖普通 macOS 用户权限，不等于需要 App Sandbox；本版本
是 Developer ID 站外分发，不启用 Mac App Store sandbox。

### 5.3 嵌套 Runtime

`prepare-sidecar.mjs` 继续只负责从 release target 复制并命名架构匹配的 external binary，不在准备
阶段 ad-hoc 签名。Tauri bundler 在 App 签名时处理嵌套 binary。构建后必须单独定位并检查 App 内
实际 Runtime 路径，确认：

- Mach-O 架构为 `arm64`；
- identity 与 App 一致；
- secure timestamp、runtime flag 和 designated requirement 有效；
- 没有因复制、strip 或二次改包使签名失效。

任何签名完成后的 App 内容修改都会使产物失效；准备 sidecar、前端资源和 Info.plist 必须全部发生在
签名之前。

## 六、正式构建流程

### 6.1 构建前检查

1. 工作区源版本和 `Cargo.lock`/`package-lock.json` 已确定；不要求工作树绝对干净，但必须列出并确认
   本次 bundle 使用的源改动，避免把未知产物发布。
2. Node、Rust、Xcode Command Line Tools、Tauri CLI 和目标 `aarch64-apple-darwin` 可用。
3. `security find-identity` 精确命中一个预期 Developer ID identity。
4. 公证环境变量齐全、`.p8` 路径存在且权限合理；不输出内容。
5. 旧 `target/release/bundle` 产物不作为本次证据；构建脚本使用当前运行生成的明确路径和时间戳。

### 6.2 构建

正式入口仍通过仓库脚本调用 Tauri release build，使 sidecar 准备和 bundle 配置一致执行。Tauri 在有
签名与公证环境时完成：

```text
build arm64 Runtime -> prepare external binary -> build Desktop assets
-> assemble App -> sign nested code -> sign App -> App notarization/staple
-> create/sign DMG -> submit final DMG notarization
-> wait Accepted -> staple DMG -> independent verification
```

公证等待必须有合理上限并保留 submission ID。超时不是成功；可以稍后通过 submission ID 只读查询，
但不能把尚未确认的产物发布。

### 6.3 公证结果

只有 Apple 返回 `Accepted` 才进入 stapling/发布。无论成功或失败，都用 `notarytool log` 检查结果：

- 失败：保存脱敏后的 issue 路径、architecture、code 和 message，停止后续发布；
- 成功：确认日志没有未处理警告，记录 submission ID；
- 不因本机 `spctl` 一次通过就跳过公证日志和 stapler 验证。

## 七、独立验证门禁

新增 release-only 验证脚本（文件名在开发计划中锁定，例如
`apps/desktop/scripts/verify-macos-release.mjs`）。脚本只读检查明确传入的 App/DMG 路径，不搜索并
误用“最新文件”，失败即非零退出。

### 7.1 App 与 Runtime

等价检查至少包括：

```bash
codesign --verify --strict --verbose=4 "/path/to/ez-assistant.app"
codesign -d --verbose=4 "/path/to/ez-assistant.app"
codesign --verify --strict --verbose=4 "/path/to/ez-assistant.app/.../ez-assistant-runtime"
codesign -d --entitlements :- "/path/to/ez-assistant.app"
spctl --assess --type execute --verbose=4 "/path/to/ez-assistant.app"
xcrun stapler validate "/path/to/ez-assistant.app"
```

`codesign --verify --deep` 可以作为额外诊断，但不能代替显式定位所有嵌套可执行代码的验证。脚本需要
断言 Authority 为目标 Developer ID、TeamIdentifier 正确、Runtime version flag 存在且架构为 arm64。

### 7.2 DMG

```bash
hdiutil verify "/path/to/ez-assistant.dmg"
codesign --verify --verbose=4 "/path/to/ez-assistant.dmg"
xcrun stapler validate "/path/to/ez-assistant.dmg"
spctl --assess --type open --context context:primary-signature --verbose=4 \
  "/path/to/ez-assistant.dmg"
```

随后以只读方式挂载 DMG，检查只包含预期 App/Applications 链接和设计资源，从挂载卷再次验证 App 和
嵌套 Runtime。卸载失败要报告，但不能删除用户已有卷或使用宽泛 destructive 命令。

### 7.3 安装后验证

把 App 复制到一个明确的临时 Applications 测试目录或干净测试用户环境，首次启动验证：

- Gatekeeper 不显示“无法验证开发者”或“已损坏”；
- Desktop 正常启动并发现/拉起随包 Runtime；
- Runtime discovery、loopback 鉴权、附件和模型设置基础流程可用；
- 退出 Desktop 与停止 Runtime 的现有独立生命周期不被签名改动破坏；
- 无构建机环境变量、源码目录或 release target 依赖。

最终验收优先在一台没有开发证书和源码环境的 macOS arm64 机器/干净用户中执行。构建机检查作为
必要条件，但不单独构成站外分发成功。

## 八、失败处理与恢复

| 阶段 | 失败处理 |
| --- | --- |
| identity 缺失/多义 | 构建前停止，用户修复 Keychain；不自动选择其他证书 |
| nested code 签名失败 | 停止，不用 `--deep --force` 掩盖；定位具体二进制和 entitlement |
| notarization Invalid | 获取日志，按具体 issue 修复后完整重建和重签；不复用已修改 App |
| notarization 超时 | 保存 submission ID，只读轮询；未 Accepted 不发布 |
| staple 失败 | 停止发布，核对已接受 submission 和网络；不把未 stapled DMG 标为完成 |
| Gatekeeper 拒绝 | 保留产物和验证输出用于诊断，重新构建；不移除 quarantine 作为验收手段 |

签名、公证不会修改用户业务数据库，因此不涉及数据库备份；发布脚本也不得清理 Runtime Home、用户
附件或现有安装。清理仅限本次明确的构建产物目录，并遵循仓库禁止破坏性宽路径命令的规则。

## 九、发布证据

版本验收记录必须保存或摘要记录：

- App/DMG 文件名、SHA-256、大小、构建时间和 arm64 架构；
- Developer ID Authority、Team ID、证书有效期（不含私钥）；
- App、嵌套 Runtime、DMG 的 `codesign` 验证结果；
- notarization submission ID、Accepted 状态和日志检查结论；
- App/DMG stapler validate、`hdiutil verify`、Gatekeeper 结果；
- 干净环境安装、首次启动和 Runtime sidecar 启动结果；
- 未执行项及原因。

验收记录不提交完整公证凭据环境、`.p8` 内容、Apple ID 或 app-specific password。

## 十、实施检查表

- [x] 确认 Developer ID Application identity、Team ID 和有效期
- [x] 确认公证凭据方案并完成只读连通验证
- [x] 增加最小 `Entitlements.plist` 与 Tauri macOS release 配置
- [x] 保持 sidecar 在签名前准备，确认 Tauri 对嵌套 Runtime 签名
- [x] 正式 arm64 App/DMG 构建和 Apple notarization Accepted
- [x] App 与 DMG 均完成 staple
- [x] 独立脚本逐层验证签名、架构、entitlements、ticket、DMG 与 Gatekeeper
- [x] 用户明确当前不发版，干净环境安装与最终源码发布件复验延后到实际发版时执行，不阻塞归档
- [x] 把脱敏证据写入 v0.16.0 验收记录
