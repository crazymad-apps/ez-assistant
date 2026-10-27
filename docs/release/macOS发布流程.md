# macOS Developer ID 发布流程

本文是 ez-assistant macOS arm64 站外发布的可重复操作手册。完整发布只使用
`apps/desktop` 中的 `npm run release:macos`；不从聊天记录拼接临时命令。

## 2026-09-14 v0.25.3 发布记录

- 源码标签 `v0.25.3` 指向提交 `ec834abf69b94745b889a66aca1ab6aaa43f1cf3`。用户使用本手册入口完成 macOS arm64 手工打包；最终本地 DMG 为 `target/aarch64-apple-darwin/release/bundle/dmg/EZ Assistant_0.25.3_aarch64.dmg`，大小 39,575,747 字节，SHA-256 为 `6e4d473fdaf38e53e55e1ab491401fddedd133e1d3d0890e46a3ff953a8cbd92`。
- Developer ID 签名检查通过；签名身份、证书元数据及公证任务标识仅保存在本地私有环境记录。
- App 与 DMG 公证均已从 Apple 回读为 `Accepted`。
- 独立 `verify:macos-release` 通过：App、Desktop 和内嵌 Runtime 均为 arm64，Developer ID／Hardened Runtime／entitlements／Gatekeeper／stapled ticket 有效；DMG 签名、票据、CRC、只读挂载内容和挂载后 App 验证通过。
- GitHub Release 公共文件名规范化为 `EZ-Assistant_0.25.3_macos_arm64.dmg`，同时上传同名 `.sha256`。远程资产 digest 与上述 SHA-256 一致；回下载 DMG 与本地产物逐字节一致，校验文件可直接通过 `shasum -a 256 -c`。发布页：[EZ Assistant v0.25.3](https://github.com/crazymad-apps/ez-assistant/releases/tag/v0.25.3)。
- 本次补发不重新执行 macOS 干净环境安装和交互操作；用户此前已完成人工交互验收，当前记录只把最终签名、公证产物与公开下载闭环对应起来。

## 1. 入口与边界

- 总控脚本：`apps/desktop/scripts/release-macos.mjs`
- 只读验证脚本：`apps/desktop/scripts/verify-macos-release.mjs`
- 平台配置：`apps/desktop/src-tauri/tauri.macos.conf.json`
- 生产 entitlement：`apps/desktop/src-tauri/Entitlements.plist`

总控脚本只支持 Apple Silicon Mac 上的 arm64 Developer ID 发布。Mac App Store、Universal
Binary、CI/CD 和其他平台不在当前流程内。v0.29.0 起同时生成 F01 在线更新制品及签名清单。

v0.32.1 在途 M4 将 DMG 的内容改为系统 PKG 安装器，安装目标为 `/Applications`；
原 `.app.tar.gz` 应用内更新包继续保留。以下 PKG 要求适用于该新入口，不能据此宣称 M4 或正式发布已经验收。

## 2. 首次准备

1. 在登录 Keychain 安装带私钥的 `Developer ID Application` 和 `Developer ID Installer` 证书。
   后者用于 PKG，不能用 Application 证书替代；二者必须属于同一 Team。
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

6. 配置 Tauri 更新签名。可信公钥位于
   `packages/assistant-protocol/resources/desktop-update.pub`，由 Desktop 构建嵌入并随 Center 装配。
   私钥只保存在仓库外，使用 `TAURI_SIGNING_PRIVATE_KEY_PATH` 与
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 注入；不要上传到中心或随安装包分发。
   已安装客户端会继续信任内置公钥，后续发布必须沿用对应私钥，不能每次重新生成。
   私钥文件权限为 `0600`，并独立妥善备份；实际保存位置和注入配置仅在本地私有环境记录中维护。
   正式签名不得使用历史测试密钥。

## 3. 每次发布

先确认根 `Cargo.toml` 的 `[workspace.package].version`、`apps/desktop/src-tauri/tauri.conf.json` 与
`apps/desktop/package.json` 的版本完全一致，再在当前 shell 注入凭据元数据。发布脚本会在构建和
公证前强制执行该一致性检查：

```bash
cd /absolute/path/to/ez-assistant/apps/desktop

export APPLE_SIGNING_IDENTITY='Developer ID Application: <NAME> (<TEAM_ID>)'
export APPLE_INSTALLER_SIGNING_IDENTITY='Developer ID Installer: <NAME> (<TEAM_ID>)'
export APPLE_TEAM_ID='<TEAM_ID>'
export APPLE_API_ISSUER='<ISSUER_ID>'
export APPLE_API_KEY='<KEY_ID>'
export APPLE_API_KEY_PATH='/absolute/path/to/AuthKey_<KEY_ID>.p8'
export TAURI_SIGNING_PRIVATE_KEY_PATH='/absolute/path/to/desktop.key'
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD='<PRIVATE_KEY_PASSWORD>'

npm run release:macos
```

脚本会依次执行：

1. 检查 macOS/arm64、Developer ID identity、Team ID、API Key 文件名和 `0600` 权限。
2. 构建 release Runtime sidecar 与 Desktop 前端。
3. 由 Tauri 签名嵌套 Runtime 和 App，提交 App 公证并 staple App ticket。
4. 将维护程序副本单独以 Application 证书签名并校验，再将 App 与同步安装交接脚本封装成 Developer ID Installer 签名的 PKG，再生成并签名 DMG。
5. 将最终 DMG 单独提交 Apple 公证，等待 `Accepted`，读取公证日志并 staple
   DMG ticket。
6. 运行独立验证，检查 App、所有 Mach-O、DMG、arm64 架构、Developer ID、Team ID、
   Hardened Runtime、entitlements、Gatekeeper、ticket、DMG 完整性、PKG 签名和展开后的 App 内容。
7. 从最终 App 生成不携带 AppleDouble/xattrs 的更新压缩包，读取实际随包 Host 构建信息，
   生成并签名 `release.json` 与在线制品；摘要和签名对应最终公证、装订后的文件。
8. 输出 App/DMG 绝对路径、DMG 大小、最终 SHA-256 和 DMG submission ID。

终端显示 `macOS release completed.` 且验证脚本显示
`macOS release verification passed.` 时，本机发布门禁才通过。

## 4. 产物

产物位于：

```text
target/aarch64-apple-darwin/release/bundle/macos/<PRODUCT_NAME>.app
target/aarch64-apple-darwin/release/bundle/dmg/<PRODUCT_NAME>_<VERSION>_aarch64.dmg
target/aarch64-apple-darwin/release/bundle/desktop-release/
```

只分发当次脚本最后输出路径与 SHA-256 对应的 stapled DMG。不使用旧 `target`
目录中的同名或历史产物作为本次发布证据。

本地测试可在已签名 App 上使用 `npm run package:macos -- --app <App.app> --output <新路径.dmg>
--unsigned-pkg --signing-identity <Application身份>`；这只构建外层签名、内部 PKG 未签名的测试候选，
不执行公证或发布。正式入口不接受缺少 Installer 身份的降级。

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
- HTTP 403 且提示协议缺失或过期：由 Apple 账号持有人自行确认并签署所需协议，再重新运行完整发布流程。
  已完成代码签名不能替代公证；仅供内部手动测试的包须明确标注未公证，不视为正式发布门禁通过。
- 终端中断：Apple 端的已提交任务不会自动取消，但本地自动 staple/验证会停止。优先确认
  submission 状态；若无法明确当前产物是否完整，重新执行总控脚本。

## 7. 发布证据与最终验收

每次发布至少记录：

签名身份、账号元数据和公证任务标识只写入本地私有记录；公开归档保留验证结论及可公开制品摘要。

- 版本号、DMG 文件名、大小和最终 SHA-256；
- Developer ID Authority、Team ID 和证书有效期；
- App 与 DMG submission ID、`Accepted` 状态和日志结论；
- App、嵌套 Runtime 和 DMG 的签名、ticket、Gatekeeper 与架构验证结果；
- 干净 macOS arm64 环境从 DMG 安装、首次启动 Desktop 并连接随包 Runtime 的结果。

本机脚本通过不代表可以跳过干净环境验收。验收时不得通过移除 quarantine 属性规避
Gatekeeper。
