# Client npm 发布流程

本文用于 ez-assistant Client 的 npm 发包。流程为：**确定版本和平台 → 独立构建 Host/Web → 装配 npm 包 → 隔离验收 → 平台附件发布到 next → 主包发布到 next → registry 安装验证 → 切换 latest**。

本文从 v0.25.2 M6 发布流程整理，v0.25.2 已于 2026-09-11 经用户授权发布到官方 npm 源，发布记录见下节。Desktop 的 App/DMG 继续使用[macOS 发布流程](macOS发布流程.md)，不与 npm 发布绑定。

## 2026-09-11 官方发布记录

- Registry：`https://registry.npmjs.org/`；发布账号 `crazy_mad`（组织 owner）。
- Client 与 macOS arm64、Linux arm64、Linux x64 三个平台附件均为 `0.25.2`；主包精确依赖同版本附件。四包的 `next`、`latest` 回读均为 `0.25.2`。
- 源码修订 `31546f304c38890d279f78f26d50b97cec052afc`，对应已推送标签 `v0.25.2`。Client 重新编译；Host 从已验收私有附件提取，并逐字节核对二进制及 manifest，未使用来源不确定的 target 产物。
- 发布前四包 dry-run、SHA512 和 macOS 代码签名检查通过。发布后四包 registry SHA1／SHA512 与本地产物一致。
- 使用独立 prefix 与空缓存从官方源安装主包，确认仅安装本机 macOS arm64 附件，安装后二进制与验收产物一致；Node 22.12.0 执行 CLI 返回 `0.25.2`，Host build-info 返回软件／最低兼容版本均为 `0.25.2`。本次未重新运行 Linux 全量验收、Desktop 构建或业务 Host，也未修改生产环境。
- 本地证据：`/tmp/ez-client-npm-official-v0252-verified/official-preflight.json`、同目录 `official-verification.json`；隔离安装目录 `/tmp/ez-client-official-registry-install-0252`。这些临时路径不作为长期归档保证。

安装命令：

```sh
npm install -g @ez-assistant/client@0.25.2 --registry=https://registry.npmjs.org/
ez-assistant config
ez-assistant start
```

GitHub 同版本发行页：[EZ Assistant v0.25.2](https://github.com/crazymad-apps/ez-assistant/releases/tag/v0.25.2)，已附 macOS arm64 Desktop DMG 和上述 npm 安装信息。Desktop DMG SHA-256 为 `96af0485b444c31562384abcc488d517c18c0c324441573088ea11a6c44f8ae7`；上传后 GitHub 资产摘要核对一致。

## 1. 发哪些包

| 包名 | 内容 | 发布要求 |
| --- | --- | --- |
| `@ez-assistant/client` | 编译后的 Client、生产依赖、`ez-assistant` 命令入口 | 最后发布；精确依赖同版本平台附件 |
| `@ez-assistant/client-darwin-arm64` | macOS arm64 Host，内嵌 Web | 完成 Release 构建、有效代码签名及真实 npm 安装运行验证 |
| `@ez-assistant/client-linux-arm64` | Linux arm64 glibc Host，内嵌 Web | 对应 Linux 平台实测 |
| `@ez-assistant/client-linux-x64` | Linux x64 glibc Host，内嵌 Web | 对应 Linux 平台实测 |

平台附件是 Client 的组成部分，用户只安装主包。共享 `@ez-assistant/protocol` 已以 Node 编译产物随主包打入，不单独发布；Node 由用户预装，当前支持 `^22.12.0 || ^24.0.0`。

正常发布四个包使用同一软件版本。应用最低兼容版本来自根 Cargo 发布声明，同版本组件保持一致；数据库最低兼容 Host 版本按存储设计独立核对，不能为了包回退而降低下限。版本生成与检查见[技术方案第四、八节](../versions/v0.25.2/技术方案.md)。

本项目当前应用软件版本解析仅支持 `x.y.z`。候选包使用正常软件版本加 npm `next` 标签，例如 `0.25.2`，不临时改成 `0.25.2-rc.1`。验证通过后给同一批文件增加 `latest` 标签，不重新打包。npm 标签独立于软件版本，未指定标签安装时默认选择 `latest`。[npm dist-tag 说明](https://docs.npmjs.com/cli/v11/commands/npm-dist-tag/)

### 私有仓库的 Client 单独修订

已上传包不覆盖、不删除后重发。仅 Client 修复且 Host 内容不变时，可在装配命令前设置 `EZ_ASSISTANT_NPM_CLIENT_REVISION=1`，生成新的主包 npm 版本 `0.25.2-1`；平台附件仍精确依赖 `0.25.2`，无需重复发布。该后缀只属于 npm 分发包号，不传入应用握手、Host 清单或数据库版本；`ez-assistant --version` 仍显示应用软件版本 `0.25.2`。正式发布默认不设置此变量。回读时分别核对主包 npm 版本和平台依赖版本，不要求修订主包与未变化附件的 npm 包号一致。

## 2. 发布前准备

以下命令从仓库根目录执行，路径替换为实际绝对路径。每个平台构建使用同一待发布源码修订和锁文件；记录 Git revision、工作区差异、Node/npm/Rust 版本和构建平台。正式产物必须能对应已验收的源码，不直接沿用未知来源的历史 target 文件。

```sh
node --version
npm --version
rustc --version
npm ci --prefix packages/assistant-protocol
npm ci --prefix apps/client
npm run check:version --prefix apps/client
```

发布构建基准为 Node 24.21.0，运行下限为 Node 22.12.0（Commander 15 要求），兼容验证使用真实最低版本和 Node 24。Node 22.12–22.14 使用默认 CA 与 `NODE_EXTRA_CA_CERTS`，22.15+ 额外导入系统 CA；始终校验证书。版本变更应先同步 Cargo、Client、Desktop、协议包、锁文件和生成的发布常量，再执行版本检查；不要只运行 `npm version` 修改 Client 一个包。生成协议使用现有 `npm run generate:protocol --prefix apps/desktop`，不手改生成文件。

首次公开发布先确认 `@ez-assistant` scope 归属及四个包的发布权限。若 scope 不可用，先统一修改分发包名、平台解析和文档，再重新验收，不能在发布命令中临时换名字。认证由发布者在本机或已配置的 CI 完成，不把 token/OTP 写进仓库或日志。

```sh
npm config get registry
npm config get @ez-assistant:registry
npm whoami --registry=https://registry.npmjs.org/
```

若 scope 配置指向其他 registry，先确认实际发布目的地；不能把登录成功当成具备组织发布权限。已有包还应核对 owners／访问权限和现有版本。公开包命令显式指定 `--access public`，并满足 npm 账号认证要求。[公开 scoped 包发布说明](https://docs.npmjs.com/creating-and-publishing-scoped-public-packages/)

## 3. 独立构建每个平台的 Host/Web

在各自构建环境安装 Web 构建依赖，并使用已有独立入口。该命令只构建 Web 和 Rust Host；Linux 不构建或运行 Desktop GUI。以下三条构建命令分别在对应平台执行，不在一台机器上顺序冒充三个原生平台的验证。

```sh
npm ci --prefix apps/desktop

# macOS arm64 构建环境
npm run build:host --prefix apps/desktop -- --release --locked --target aarch64-apple-darwin

# Linux arm64 glibc 构建环境
npm run build:host --prefix apps/desktop -- --release --locked --target aarch64-unknown-linux-gnu

# Linux x64 glibc 构建环境
npm run build:host --prefix apps/desktop -- --release --locked --target x86_64-unknown-linux-gnu
```

默认输出分别为 `target/<target>/release/ez-assistant-runtime`；设置过 `CARGO_TARGET_DIR` 时，以实际构建输出为准。在每个目标系统对实际文件执行 `--build-info-json`，核对软件版本与最低兼容版本，同时核实嵌入 Web 可用。构建和清单架构检查不能替代目标系统运行。

macOS npm 渠道不把 Developer ID 签名和 Apple 公证设为无条件发布前置。Apple Silicon 的基础代码签名要求与公证不同：有效 ad-hoc 签名可以满足基础执行要求，工具链可以在构建时生成；应对最终 Host 执行 `codesign --verify --strict <Host路径>` 并记录签名类型。[Apple 对工具代码签名的说明](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app)

发布门槛是正式 Release、支持的 macOS／架构，以及真实 npm 下载、解包及包内 Host 启动路径的验证。检查安装文件的实际 quarantine 属性与系统反馈；终端调用和 Finder 打开可走不同检查路径，不能直接套用 Desktop App/DMG 的验收，也不能声称 npm 二进制免受 macOS 安全检查。[Apple 对命令行工具 Gatekeeper 行为的说明](https://developer.apple.com/forums/thread/706379)

Apple 建议对站外分发软件公证。Developer ID 加公证可以作为发行信任措施；若以后增加浏览器直接下载、安装器或其他交付方式，再按实际渠道确定要求。[Apple 分发建议](https://developer.apple.com/news/?id=saqachfa)。如果采用额外签名／公证流程，需在装配最终 npm 包和生成摘要之前完成；验收后不可再签名或修改包内 Host。Desktop 原有 App/DMG 发布要求保持独立。


## 4. 一次性装配候选包

收集同一发布批次的三个最终 Host。下面的路径必须替换；输出目录必须不存在，脚本不会覆盖旧包。

```sh
export EZ_NPM_RELEASE_VERSION='0.25.2'
export EZ_NPM_RELEASE_DIR='/absolute/path/to/releases/client-0.25.2'

node apps/client/scripts/pack-npm.mjs "$EZ_NPM_RELEASE_DIR" \
  darwin-arm64=/absolute/path/to/macos/ez-assistant-runtime \
  linux-arm64=/absolute/path/to/linux-arm64/ez-assistant-runtime \
  linux-x64=/absolute/path/to/linux-x64/ez-assistant-runtime
```

环境变量只便于后续命令引用，不会修改版本源；必须与 `pack-report.json` 中的版本一致。脚本会重新编译 Client、按锁文件在隔离目录安装生产依赖、装配平台附件和主包，再执行 npm pack；不会发布、启动 Host 或修改系统服务。输出包括：

```text
client-0.25.2/
  ez-assistant-client-0.25.2.tgz
  ez-assistant-client-darwin-arm64-0.25.2.tgz
  ez-assistant-client-linux-arm64-0.25.2.tgz
  ez-assistant-client-linux-x64-0.25.2.tgz
  pack-report.json
```

以脚本输出的实际文件清单为准。脚本允许只提供部分平台用于开发验证，**不会保证正式平台齐全**；发布者必须核对主包 `optionalDependencies`、三个附件及其版本完全一致。当前脚本始终写 `releaseReady: false`，不存在自动转成可发布的开关；发布资格以版本验收记录为准，不手改该字段绕过缺口。

装配报告核对包内容、生产依赖、bin、engines、os/cpu/libc、版本和 Host SHA256。项目源码、测试、开发依赖、仓库 `file:` 引用、用户配置及凭据不得进入产品包。依赖自己的运行 JS 即使位于其 src 目录，也不等于项目 TypeScript 源码。当前脚本使用 `UNLICENSED` 元数据，尚未完成 Host/Web 第三方许可证汇总；先明确产品许可并补齐必须分发的许可证，再生成正式候选包，不能用修改 tgz 的方式补文件。

在输出目录生成并留存完整包摘要：

```sh
node --input-type=module - "$EZ_NPM_RELEASE_DIR" <<'NODE'
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
const root = process.argv[2];
const files = readdirSync(root).filter(name => name.endsWith('.tgz')).sort();
const rows = files.map(file => ({file, sha256: createHash('sha256').update(readFileSync(join(root, file))).digest('hex')}));
writeFileSync(join(root, 'sha256.json'), JSON.stringify(rows, null, 2) + '\n');
NODE
```

后续 dry-run、发布和 registry 回读都针对这批原始 `.tgz`。主工程 `apps/client/package.json` 保留 `private: true`，不要在源码目录直接执行不带 tarball 参数的 `npm publish`，也不要发布 `pack-report.json` 所指向的临时工作目录。

## 5. 候选验收与发布预检

对每个拟发布目标执行成品包验收：

```sh
node apps/client/tests/npm-distribution-smoke.mjs "$EZ_NPM_RELEASE_DIR"

# 使用上一个命令实际输出的隔离 prefix，不使用生产安装
node apps/client/tests/npm-package-failures.mjs \
  '/absolute/path/to/ez-npm-测试目录/prefix/lib/node_modules/@ez-assistant/client'
```

Linux 的运行依赖和 Docker 驱动方式见[Linux 成品包验收](../../apps/client/tests/linux/README.md#m6-npm-候选包验收)。仅拷贝成品包和 Node/npm 工具链进入运行容器，测试驱动可通过 stdin 执行。Linux systemd 验证需专用普通用户的 manager／D-Bus／linger 条件；不要为了让测试通过而改生产账号。

该驱动创建隔离 prefix 和 Runtime Home，选择空闲端口；验证安装、只读查询、先停止再重装，以及卸载后包内 Host 删除、再次安装后的管理能力，并核验新建测试库和独立备份。失败时保留证据、查询实际状态，不扩大为生产修复。它当前验证的是**同版本重装**，而且版本断言固定为 `0.25.2`；后续版本需先更新驱动的版本引用，不能直接宣称通用跨版本升级已验证。

还需按版本计划补齐：真实跨软件版本升级、协议／数据库不兼容拒绝、独立构建与 Desktop 共存、服务开机路径、实际 Web 功能。复用已有证据须能对应本批产物和未变代码；不为发包重复无关全量测试，也不能用旧包的结果冒充改变后的文件已验收。

对每个 tarball 分别 dry-run，并与 pack-report.json 核对名称、版本、内容和大小：

```sh
npm publish "$EZ_NPM_RELEASE_DIR/ez-assistant-client-linux-arm64-$EZ_NPM_RELEASE_VERSION.tgz" \
  --dry-run --ignore-scripts --access public --tag next --registry=https://registry.npmjs.org/
```

上面仅展示其中一个包，其余三个同样执行。dry-run 不代表权限、registry 上传或安装已通过；只能用于发布预检。[npm publish 说明](https://docs.npmjs.com/cli/v11/commands/npm-publish/)

预检完成后形成可审阅的发布记录：源码修订、平台范围、包名与版本、SHA256／npm integrity、验收结果、许可证和签名证据、现有 latest/next 指向、待发布命令。实际执行发布前须具备该版本的发布授权；本手册不提前推进 M7/M8 或代替版本确认。

## 6. 发布到 next：附件先发，主包最后

`next` 仍是公开发布，所有上传包都可能被指定版本安装；它不提供私有测试隔离。首次发布同样显式使用 next，并回读标签，不能假设首次包一定已有旧 latest 可保留。

按以下顺序逐包执行，每一步成功后核对 registry 返回，不把它们包装成遇错仍继续的批量命令：

```sh
npm publish "$EZ_NPM_RELEASE_DIR/ez-assistant-client-darwin-arm64-$EZ_NPM_RELEASE_VERSION.tgz" \
  --ignore-scripts --access public --tag next --registry=https://registry.npmjs.org/

npm publish "$EZ_NPM_RELEASE_DIR/ez-assistant-client-linux-arm64-$EZ_NPM_RELEASE_VERSION.tgz" \
  --ignore-scripts --access public --tag next --registry=https://registry.npmjs.org/

npm publish "$EZ_NPM_RELEASE_DIR/ez-assistant-client-linux-x64-$EZ_NPM_RELEASE_VERSION.tgz" \
  --ignore-scripts --access public --tag next --registry=https://registry.npmjs.org/
```

每个附件发布后查询实际版本的 `dist.integrity`／`dist.shasum`，与 pack-report.json 同名产物比较。所有附件存在且一致后，才发布主包：

```sh
npm view "@ez-assistant/client-linux-arm64@$EZ_NPM_RELEASE_VERSION" version dist --json --registry=https://registry.npmjs.org/
# darwin-arm64、linux-x64 同样回读通过后，再执行下面的主包发布
npm publish "$EZ_NPM_RELEASE_DIR/ez-assistant-client-$EZ_NPM_RELEASE_VERSION.tgz" \
  --ignore-scripts --access public --tag next --registry=https://registry.npmjs.org/

npm view "@ez-assistant/client@$EZ_NPM_RELEASE_VERSION" \
  version optionalDependencies dist --json --registry=https://registry.npmjs.org/
npm view @ez-assistant/client dist-tags --json --registry=https://registry.npmjs.org/
```

主包可能在平台附件缺失时仍被 npm 安装，因此“主包上传成功”不能说明产品可运行。任何上传超时或结果不明，先查询该 name/version 与远端摘要：已经存在且一致则记为已完成，不重复上传；不存在才根据错误继续处理；存在但不同则停止。npm 的同名同版本不可覆盖，即使 unpublish 也不能重新使用该组合。[npm 版本唯一性约定](https://docs.npmjs.com/cli/v11/commands/npm-publish/)

## 7. 从 registry 安装验证，再提升 latest

在每个目标平台使用新的普通用户隔离前缀，从实际 registry 安装，确认远端主包的当前平台附件也被安装：

```sh
export EZ_NPM_VERIFY_ROOT="$(mktemp -d)"
export EZ_NPM_VERIFY_PREFIX="$EZ_NPM_VERIFY_ROOT/prefix"

npm install -g "@ez-assistant/client@$EZ_NPM_RELEASE_VERSION" \
  --prefix "$EZ_NPM_VERIFY_PREFIX" --cache "$EZ_NPM_VERIFY_ROOT/cache" \
  --ignore-scripts --registry=https://registry.npmjs.org/

"$EZ_NPM_VERIFY_PREFIX/bin/ez-assistant" --version
"$EZ_NPM_VERIFY_PREFIX/bin/ez-assistant" --runtime-home "$EZ_NPM_VERIFY_ROOT/home" status
```

选择可执行的临时文件系统；Linux `/tmp` 为 noexec 时使用专用用户私有测试目录。上述命令只验证版本和只读状态；完整功能仍须在这个 registry 安装副本完成：config 设置空闲测试端口 → start → Web → 关闭自启并 stop → npm 卸载／重装 → start／restart → stop。每次都显式传隔离 Runtime Home，不沿用默认生产目录。验收还应确认 `@next` 与本次准确版本一致；安装版本成功不等于标签正确。

需要逐字核对时，使用 `npm pack <包名>@<版本> --pack-destination <新的回读目录>` 下载 registry 包，并比较其 SHA256 与发布前记录。不能在原产物目录下载并覆盖原始文件。

验收和发布确认都具备后，依次更新三个附件的 latest，最后更新主包 latest：

```sh
npm dist-tag add "@ez-assistant/client-darwin-arm64@$EZ_NPM_RELEASE_VERSION" latest --registry=https://registry.npmjs.org/
npm dist-tag add "@ez-assistant/client-linux-arm64@$EZ_NPM_RELEASE_VERSION" latest --registry=https://registry.npmjs.org/
npm dist-tag add "@ez-assistant/client-linux-x64@$EZ_NPM_RELEASE_VERSION" latest --registry=https://registry.npmjs.org/
npm dist-tag add "@ez-assistant/client@$EZ_NPM_RELEASE_VERSION" latest --registry=https://registry.npmjs.org/
```

每一步都回读对应包的 dist-tags；最后从全新隔离前缀执行不带版本的 `npm install -g @ez-assistant/client`，确认默认安装落到本次版本及正确平台。主包依赖精确版本，附件标签不是运行时版本选择依据。保留 next 指向本次版本即可，后续候选发布再移动它。[标签更新与默认安装行为](https://docs.npmjs.com/cli/v11/commands/npm-dist-tag/)

## 8. 中断恢复、撤回推荐与用户回退

| 情况 | 处理 |
| --- | --- |
| 部分平台上传成功，主包尚未上传 | 保留已上传附件，记录 name/version/integrity；修复上传条件后继续缺少的步骤。源码或附件内容若需修改，重新确定版本并构建验收整批包 |
| 主包已在 next，验收失败 | 不提升 latest；记录问题，必要时标记弃用。修复发布必须使用新版本，不能覆盖已上传的 tgz |
| 提升标签时网络中断 | 回读四个包标签，按实际状态继续。主包 latest 尚未切换前不宣称稳定渠道已更新 |
| latest 版本有问题 | 将主包 latest 指回发布前记录的可用版本，并回读、验证默认安装；需要保持附件标签一致时分别调整。不要盲目把所有附件设为一个从未发布过的旧版本 |
| 首次发布没有可用旧版本 | 没有可回退的稳定版本；说明当前故障、标记弃用并尽快发修复版，不能声称恢复了旧版 |

有真实可用旧版本时，撤回推荐的命令示例：

```sh
export EZ_NPM_PREVIOUS_VERSION='<发布前记录且已验证的旧版本>'
npm dist-tag add "@ez-assistant/client@$EZ_NPM_PREVIOUS_VERSION" latest --registry=https://registry.npmjs.org/
npm view @ez-assistant/client dist-tags --json --registry=https://registry.npmjs.org/
```

需要提示指定坏版本的用户时，可对准确的包和版本执行 `npm deprecate '<包名>@<坏版本>' '具体问题与替代版本说明'`；不使用会覆盖其他版本的宽泛范围。弃用会给安装者提示，不会自动卸载或升级用户机器。[npm deprecate 说明](https://docs.npmjs.com/cli/v11/commands/npm-deprecate/)

修改 npm 标签只影响之后的版本选择，不会替用户停止或降级 Host。重新安装旧 Client 仍须通过应用协议兼容检查；npm 升级／卸载会替换或删除包内 Host，操作前显式停止，卸载前关闭自启；不会自动删除 unit 或共享数据。切换 Host 必须显式停止、更新来源并重新启动；旧 Host 若低于数据库最低兼容版本，就拒绝启动，不能通过修改兼容下限或恢复数据库来“完成 npm 回退”。不以 npm unpublish 作为默认故障恢复方式。

## 9. 发布记录与当前缺口

每次保留：源码修订与版本声明、各平台构建日志、原始 tgz、pack-report.json、包摘要、目标系统与测试结果、签名及许可证证据、registry integrity 回读、四个包发布及标签更新结果、原 latest/next 指向、已知限制和用户安装说明。归入当次版本验收／归档记录，凭据不归档。

截至2026-09-10，v0.25.2 尚不能直接按本流程公开发包：

- Linux arm64 Release npm 附件和 macOS arm64 安装机制验证通过；macOS Release 附件已构建并核验有效 ad-hoc 签名，已发布私有仓库。Linux x64 Release 附件已构建，成品在 QEMU 10.2.1 + CentOS 7 glibc 2.17 库环境通过版本／帮助检查，ELF 最高要求 GLIBC_2.17；真实 x64 系统安装、业务与服务行为仍待用户验证。公证未执行，但不再作为 npm 渠道的必选门槛。
- 当前装配脚本尚未完成 Host/Web 许可证汇总与正式产品许可元数据；`releaseReady: false` 不由脚本自动放行。
- 本地临时 registry 的安装／重装／卸载已验证，实际 npm scope 权限、公开发布、远端安装及跨软件版本 npm 升级尚未验证。
- A21 真实整机无人登录开机、A23 新干净检出独立构建，以及版本计划其余发布前证据仍需补齐或按流程明确处理；M6 进行中，M7/M8 未开始。

当前实施和已验证范围见[开发计划 M6](../versions/v0.25.2/开发计划.md#m6独立分发升级卸载与架构替换验证)。后续自动化可按本流程封装，但目前没有已实现的“一键公开发布”脚本；已有 `pack-npm.mjs` 只负责装配。

2026-09-10 澄清：此前文档把 macOS 公证写成 npm 发包的无条件要求，混淆基础代码签名、实际安装触发的系统检查与发行信任策略。本次据实际渠道修订，仍保留 Release 与目标平台运行验证，不将本机 debug 成功等同于正式发布通过。

2026-09-10 x64 候选：`/tmp/ez-client-npm-m6-x64` 已生成 `@ez-assistant/client-linux-x64@0.25.2`（24,258,199 字节）和增加对应精确可选依赖的主包 `0.25.2-4`（233,332 字节）。两份既有 arm64 附件摘要不变，无需重发。私有仓库 `172.16.20.4:40087` 当前 curl 返回空响应、npm ping 返回 ECONNRESET，尚未上传这两份新包；恢复连接后先发 x64 附件，回读摘要，再发主包 `next`，保留原版本，不主动提升 `latest`。

x64 构建使用 macOS arm64、nightly Rust、cargo-zigbuild 0.23.3／Zig 0.16.0，目标 `x86_64-unknown-linux-gnu.2.17`，沿用已构建的 0.25.2 Web。glibc 2.17 验证仅覆盖 Host 动态加载与无副作用命令，不代表旧 Linux 内核、完整 Client 或 systemd 验收；Client 仍要求 Node `>=24.21.0 <25`，而[官方 Node 24 Linux 二进制要求 glibc >=2.28](https://nodejs.org/en/blog/migrations/v22-to-v24)。服务器现有 Node 22.13.0 不满足 Client 要求。

本轮私有发布目标随后由用户切换为家庭仓库 `http://192.168.31.21:4873/`。初次检查三个平台附件 `0.25.2` 与主包 `0.25.2-4` 均不存在，首包发布曾因未登录返回 `ENEEDAUTH`。用户完成登录后，2026-09-11 已按 macOS arm64、Linux arm64、Linux x64、主包的顺序全部发布成功；每包回读 SHA1／SHA512 与候选一致，主包精确依赖三个 `0.25.2` 附件。发布命令均使用 `next`，但 Verdaccio 首次发布自动生成了 `latest`：三个附件的 `next/latest=0.25.2`，主包的 `next/latest=0.25.2-4`，未另外执行标签提升。回读证据为候选目录内 `home-registry-verification.json`。不修改全局 registry，未在用户环境安装或运行 Host，安装验收由用户手动完成。

2026-09-11 Node 22 兼容修订：主包 `0.25.2-5` 已发布到上述家庭仓库，`next=0.25.2-5`，`latest` 保留 `0.25.2-4`。`engines` 和入口统一为 `^22.15.0 || ^24.0.0`；Node 22.15／24.21 macOS 定向测试各 21 项、Node 22.15 Linux 成品包定向测试 21 项、Node 22.15 交互菜单／取消检查通过。系统／附加 CA、证书拒绝和原生 shebang 均有实际执行证据。平台附件摘要不变，未重发；候选与 registry 回读证据位于 `/tmp/ez-client-npm-m6-node22`。本次没有真实业务 Host／数据库操作或安装验收，未执行全量回归。使用显式 `0.25.2-5` 或 `next` 获取修订；Node 22.13 仍低于 API 下限。

### 2026-09-11 统一访问规则私有修订

主包 `0.25.2-6` 与三个平台附件 `0.25.2-1` 已发布家庭仓库 `http://192.168.31.21:4873/` 的 `next`；主包 `latest=0.25.2-4`、附件 `latest=0.25.2` 均未改变。应用软件版本与最低兼容仍是 `0.25.2`，包修订号不参与应用协议兼容判定。

Host 内容变更时必须同时生成新的平台附件包号：packer 支持 `EZ_ASSISTANT_NPM_HOST_REVISION`，与既有 `EZ_ASSISTANT_NPM_CLIENT_REVISION` 分别控制附件和主包 npm 修订号。此次分别为 1 和 6。主包精确依赖附件包号；安装来源校验按主包声明的精确依赖核对，Host manifest／build-info 则继续核对应用版本和摘要。只更新 Client 时可复用已发布附件，不可把包修订号误当应用版本，也不能只发主包而保留有缺陷的旧 Host。

候选及证据位于 `/tmp/ez-client-npm-m6-unified-access`，包括 pack-report、SHA256、发布前标签及 dry-run、逐包 publish 日志和 registry SHA1/SHA512／依赖回读。更新前使用实际 Runtime Home 停止已有 Host；升级后再启动。Desktop 不发布安装包，用户使用普通 `npm run tauri -- dev` 联调。

2026-09-11 按用户指定同步发布公司仓库 `http://172.16.20.4:40087/`：复用上述相同四个 tgz，不重新构建。主包 `0.25.2-6`、三个附件 `0.25.2-1` 已发布 `next`；逐包 SHA1/SHA512、主包精确依赖和标签回读通过。主包和两个 arm64 附件的 `latest` 保持 `0.25.2`；linux-x64 是该仓库首次发布，Verdaccio 自动生成 `latest=0.25.2-1`，未额外修改标签。公司发布证据独立位于 `/tmp/ez-client-npm-m6-unified-access/company`，家庭仓库证据保留。安装验证由用户执行，Desktop 不发包。


2026-09-11 Node 下限继续调整（M6）：用户确认后将运行范围改为 `^22.12.0 || ^24.0.0`，Commander 15 为最低版本依据。Node 22.12–22.14 使用默认 CA 与 `NODE_EXTRA_CA_CERTS`，22.15+ 继续自动合并系统 CA；所有版本保持链与主机名校验。
Client 主包 `0.25.2-7` 已发布公司仓库 `http://172.16.20.4:40087/`，回读确认 `next=0.25.2-7`，`latest=0.25.2` 保持不变，三平台附件仍精确依赖 `0.25.2-1`，归档摘要与之前一致且未重发。应用软件版本仍为 0.25.2。
验证：构建通过；macOS arm64 真实 Node 22.12.0（官方 SHA256 核对）和 Node 24.21.0 的入口／HTTPS 定向测试各 3 项通过；Node 22.12 成品包来源、摘要与启动路径检查通过（拦截 launch，没有启动业务 Host）。未执行 Linux 新轮实机验收、全量回归或 Desktop 发版；安装验证由用户进行。
候选与发布回读证据：`/tmp/ez-client-npm-m6-node2212`；工具链：`/tmp/ez-client-node2212`。本轮未操作生产环境或数据库，仍处于 M6。


2026-09-11 M6 手动启动修复：用户在 root 会话执行 start 遇到用户 systemd 管理器不可用。移除以 /run/systemd/system 存在为依据的手动管理门禁：查询不可用时读取固定名称的 Client unit，确认不存在后允许普通启停；已有、不安全或不可读注册及查询冲突仍拒绝，自启状态保持未知。未设置用户总线、启用 linger 或操作服务器服务。
构建及 Node 22.12 的 systemd 生命周期／状态定向测试 7 项通过，其中回归用例模拟系统 systemd 存在但用户管理器不可用，并使用真实临时 unit 验证已注册、不安全权限、悬空链接和冲突分支。没有执行全量回归或真实服务器启动验收。
主包 0.25.2-8 已发布 http://172.16.20.4:40087/，回读确认 next=0.25.2-8，latest=0.25.2 未改变；三个平台附件继续使用 0.25.2-1，发布前核对 registry 摘要一致，未重发附件。证据 /tmp/ez-client-npm-m6-systemd-manual/company。仍处 M6，未操作生产 Host、Desktop 或数据库。


2026-09-11 M6 系统级 systemd：root 自动选 system scope，unit 位于 /etc/systemd/system，Type=simple、User=0、multi-user.target；普通用户保留 user scope、Type=exec 和 linger。scope 贯穿查询、预览、提交及生命周期；系统级不依赖 XDG_RUNTIME_DIR、用户总线和 loginctl，短锁目录 /run/ez-assistant-client 只在保存时创建。固定名称另一范围注册存在时拒绝隐式迁移。默认不开自启，不切换 Home 或运行身份，不替代 Desktop 自启。
验证：构建通过，Node 22.12 定向状态／生命周期测试 8 项通过。独立 Docker 容器 ez-client-system-service-check（无宿主挂载、systemd 255、Node 22.15、Linux arm64），直接使用成品 tgz：root 预览无写入、注册／enable／start／restart／stop／disable 和跨范围冲突检查通过；普通用户 eztest 注册／enable／disable 通过。root 的全新测试 Home=/var/lib/ez-system-service-check，实际 Host 只在容器中运行；未执行直接 SQL 操作或迁移既有数据。最终所有测试 unit disabled、Host 已停止，容器已停止并保留证据。未在公司生产服务器修改服务，未执行全量回归、整机开机或旧 systemd/cgroup v1 验收。
候选 /tmp/ez-client-npm-m6-system-service；主包版本 0.25.2-9，Host 附件保持 0.25.2-1。发布回读见 company/private-registry-verification.json。测试脚本 apps/client/tests/linux/system-service-smoke.mjs。仍处 M6。
