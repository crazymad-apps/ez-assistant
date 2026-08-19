# macOS Developer ID 发布流程

本文是 ez-assistant macOS arm64 站外发布的可重复操作手册。完整发布只使用
`apps/desktop` 中的 `npm run release:macos`；不从聊天记录拼接临时命令。

## 1. 入口与边界

- 总控脚本：`apps/desktop/scripts/release-macos.mjs`
- 只读验证脚本：`apps/desktop/scripts/verify-macos-release.mjs`
- 平台配置：`apps/desktop/src-tauri/tauri.macos.conf.json`
- 生产 entitlement：`apps/desktop/src-tauri/Entitlements.plist`

总控脚本只支持 Apple Silicon Mac 上的 arm64 Developer ID 发布。Mac App Store、Universal
Binary、CI/CD、自动更新和其他平台不在当前流程内。

## 2. 首次准备

1. 在登录 Keychain 安装带私钥的 `Developer ID Application` 证书。
2. 创建 App Store Connect Team API Key，不使用 Individual API Key。
3. 把 `AuthKey_<KEY_ID>.p8` 保存到仓库外的用户私有目录。
4. 限制目录和私钥权限：

```bash
chmod 700 /absolute/path/to/private-directory
chmod 600 /absolute/path/to/private-directory/AuthKey_<KEY_ID>.p8
```

5. 可选地为查询和故障恢复保存 `notarytool` Keychain profile：

```bash
xcrun notarytool store-credentials "ez-assistant-notary" \
  --key /absolute/path/to/AuthKey_<KEY_ID>.p8 \
  --key-id <KEY_ID> \
  --issuer <ISSUER_ID>
```

证书私钥、`.p8` 内容、Key ID 和 Issuer ID 不得写入仓库、Markdown、shell 脚本或验收日志。

## 3. 每次发布

先确认根 `Cargo.toml` 的 `[workspace.package].version`、`apps/desktop/src-tauri/tauri.conf.json` 与
`apps/desktop/package.json` 的版本完全一致，再在当前 shell 注入凭据元数据。发布脚本会在构建和
公证前强制执行该一致性检查：

```bash
cd /absolute/path/to/ez-assistant/apps/desktop

export APPLE_SIGNING_IDENTITY='Developer ID Application: <NAME> (<TEAM_ID>)'
export APPLE_TEAM_ID='<TEAM_ID>'
export APPLE_API_ISSUER='<ISSUER_ID>'
export APPLE_API_KEY='<KEY_ID>'
export APPLE_API_KEY_PATH='/absolute/path/to/AuthKey_<KEY_ID>.p8'

npm run release:macos
```

脚本会依次执行：

1. 检查 macOS/arm64、Developer ID identity、Team ID、API Key 文件名和 `0600` 权限。
2. 构建 release Runtime sidecar 与 Desktop 前端。
3. 由 Tauri 签名嵌套 Runtime 和 App，提交 App 公证并 staple App ticket。
4. 生成并签名 DMG。
5. 将最终 DMG 单独提交 Apple 公证，等待 `Accepted`，读取公证日志并 staple
   DMG ticket。
6. 运行独立验证，检查 App、所有 Mach-O、DMG、arm64 架构、Developer ID、Team ID、
   Hardened Runtime、entitlements、Gatekeeper、ticket、DMG 完整性和只读挂载内容。
7. 输出 App/DMG 绝对路径、DMG 大小、最终 SHA-256 和 DMG submission ID。

终端显示 `macOS release completed.` 且验证脚本显示
`macOS release verification passed.` 时，本机发布门禁才通过。

## 4. 产物

产物位于：

```text
target/aarch64-apple-darwin/release/bundle/macos/<PRODUCT_NAME>.app
target/aarch64-apple-darwin/release/bundle/dmg/<PRODUCT_NAME>_<VERSION>_aarch64.dmg
```

只分发当次脚本最后输出路径与 SHA-256 对应的 stapled DMG。不使用旧 `target`
目录中的同名或历史产物作为本次发布证据。

## 5. 单独复验

```bash
cd /absolute/path/to/ez-assistant/apps/desktop

npm run verify:macos-release -- \
  --app '/absolute/path/to/<PRODUCT_NAME>.app' \
  --dmg '/absolute/path/to/<PRODUCT_NAME>_<VERSION>_aarch64.dmg' \
  --team-id '<TEAM_ID>'
```

若需同时核对 Apple 公证记录，追加：

```text
--submission-id <DMG_SUBMISSION_ID> --keychain-profile ez-assistant-notary
```

## 6. 等待与失败恢复

`Notarizing` 表示产物已上传并等待 Apple 处理，不代表本地构建死锁。可在另一终端只读查询：

```bash
xcrun notarytool history --keychain-profile ez-assistant-notary
xcrun notarytool info <SUBMISSION_ID> --keychain-profile ez-assistant-notary
```

- `In Progress`：继续等待；不分发，不手工修改已签名 App/DMG。
- `Accepted`：脚本会继续 staple 和独立验证。
- `Invalid`：使用 `notarytool log` 查看具体 issue，修复后完整重新构建；不用
  `codesign --deep --force` 覆盖失败。
- 终端中断：Apple 端的已提交任务不会自动取消，但本地自动 staple/验证会停止。优先确认
  submission 状态；若无法明确当前产物是否完整，重新执行总控脚本。

## 7. 发布证据与最终验收

每次发布至少记录：

- 版本号、DMG 文件名、大小和最终 SHA-256；
- Developer ID Authority、Team ID 和证书有效期；
- App 与 DMG submission ID、`Accepted` 状态和日志结论；
- App、嵌套 Runtime 和 DMG 的签名、ticket、Gatekeeper 与架构验证结果；
- 干净 macOS arm64 环境从 DMG 安装、首次启动 Desktop 并连接随包 Runtime 的结果。

本机脚本通过不代表可以跳过干净环境验收。验收时不得通过移除 quarantine 属性规避
Gatekeeper。
