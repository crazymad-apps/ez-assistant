# EZ Assistant Client

正式 TypeScript／Node Client，管理本机共享 Runtime Host。普通操作使用命令，只有 `config` 进入交互菜单。v0.25.2 M4 已确认；M5 已接入 Client 自启配置及服务启停，使用[本机 Docker Linux 测试环境](tests/linux/README.md)验证成品包，仅测试 Client／Host／自启，不运行 Desktop。M5 按 Docker 范围已确认；整机开机仍未验证。Desktop 保留独立桌面启动逻辑。M6 以 npm 为主要分发渠道，已发布私有仓库候选包，尚未公开发布。

## 开发运行

支持 Node **22.12+（22.x）或 24.x** 和 npm，发布构建工具链仍固定为 24.21.0。依赖精确锁定为 Commander 15.0.0、Clack 1.8.0、Ora 9.4.1、Picocolors 1.1.1；无需 Bun 或 Node 原生扩展。

从仓库根目录安装、构建：

```sh
npm ci --prefix packages/assistant-protocol
npm ci --prefix apps/client
npm run build --prefix apps/client
node apps/client/dist/cli.js --help
```

协议包独立生成 Node ESM／声明文件，Client 消费 `@ez-assistant/protocol/node`。Desktop 继续消费协议包的 TS 入口；Host、Desktop、协议构建均不依赖 Client。

开发时显式指定已构建的 Host 和隔离 Runtime Home。例如先通过现有 `npm run build:host --prefix apps/desktop` 构建内嵌 Web 的 Host，再在同一个终端中运行：

```sh
export EZ_ASSISTANT_RUNTIME_EXECUTABLE="$PWD/target/debug/ez-assistant-runtime"
export EZ_ASSISTANT_RUNTIME_HOME="$(mktemp -d /tmp/ez-client-manual.XXXXXX)/home"
node --use-system-ca apps/client/dist/cli.js config
node --use-system-ca apps/client/dist/cli.js start
node --use-system-ca apps/client/dist/cli.js web
node --use-system-ca apps/client/dist/cli.js status
node --use-system-ca apps/client/dist/cli.js stop
```

在 `config` 中选择空闲端口并设置访问密码后启动。所有示例指向新建临时目录；验证后保留目录，需要停止时使用相同环境中的 `stop`。开发源码入口为 `npm run dev --prefix apps/client -- <命令>`，运行前也需完成协议构建。

npm 产品从对应平台附件定位随包 Host；开发入口需显式提供已构建的 Host 路径。M5 独立归档保留既有相邻 `host/ez-assistant-runtime` 布局，仅作为历史测试夹具。Runtime Home 选择顺序为 `--runtime-home`、`EZ_ASSISTANT_RUNTIME_HOME`、产品默认 `~/.ez-assistant`；开发和验收始终显式隔离。

## 命令行为

| 命令 | 行为 |
| --- | --- |
| 无参数／`--help`／`--version` | 显示帮助或软件版本，不读取 Host 数据 |
| `start` | 双向兼容检查，复用已有 Host；无实例时优先启动已启用的登记服务，否则调用随包 Host 启动器并等待 Ready |
| `status` | 显示就绪／初始化／失败、软件版本、当前地址、来源、待重启设置，以及系统自启状态；macOS 明确暂不支持自启 |
| `stop` | 固定原实例受控关闭，等待进程退出与锁释放；服务实例还核实 unit／cgroup 收敛；影响所有连接此 Host 的客户端 |
| `restart` | 停止前后校验原可执行文件及版本，沿原来源重启；来源失效时明确失败 |
| `config` | 在线／离线访问设置，以及独立保存的 Linux 启动设置；密码、访问配置、自启分别预览、提交和回读 |
| `web` | 已就绪且设置密码后打开普通登录地址；无图形环境输出手动地址，不传递原生 token |

`start` 默认等待 60 秒，`stop` 默认 30 秒，`restart` 分别等待 30／60 秒；`--timeout` 只能延长等待。超时或取消后用 `status` 核实，Client 不强杀 Host 或自动重放写入。退出码：成功 0、操作失败 1、参数／非交互终端错误 2、取消 130。支持 `--no-color`、`NO_COLOR` 和非 TTY 的纯文本输出；`config` 要求真实 TTY。

访问密码通过 Host 的 JSON stdin 或认证 HTTP 提交，Client 不写 TOML、不打开数据库、不保存密码哈希。离线锁核验调用 Host 的只读 `access probe`。端口预检只连接 loopback，不发送凭据；实例唯一性最终由 Host 的内核锁裁决。

HTTPS 使用系统 CA 和显式 `NODE_EXTRA_CA_CERTS`，保持证书校验，不跟随重定向、不使用环境代理。Client 生命周期与后台 Host 分离；更换 Client 框架不改变 Host 或 Desktop 的运行依赖。

Linux 在 `config → 启动设置` 开启／关闭自启或显式切换随包 Host 来源。默认关闭，root 使用系统级 systemd，普通用户使用用户级 unit 与 linger；系统级无需用户总线或 linger。开关不顺带启停 Host，关闭不撤销用户 linger。首次注册固定随包 Host，已有来源不会自动重绑；活动服务须先停止才可换来源。权限不足时按错误提示交由管理员处理，Client 不收集 sudo 密码。macOS 此项提示暂不支持。

## 验证

```sh
npm run typecheck --prefix apps/client
npm test --prefix apps/client
npm run test:terminal --prefix apps/client
npm run test:host --prefix apps/client
npm run test:port --prefix apps/client
npm run test:native --prefix apps/client
npm run test:web --prefix apps/client
```

命令测试需要 OpenSSL 生成一次性 TLS 证书。PTY／真实 Host 验收还需要 Python 3 和已构建的 Host；Web 验收需要 Desktop 的 Playwright 依赖与 Chromium，原生协同验收需要 Rust 工具链。这些仅为开发验证依赖。

macOS 验收复制 Host 并使用临时 HOME／Runtime Home，不启动安装版 GUI或安装本机服务；Linux 服务验收仅注册 Docker 测试用户的独立 unit。Web 使用本地模拟 Provider。数据库夹具逐表精确计数和字段摘要，并独立备份回读。Linux arm64 已通过成品包的 Client／Host 链路、内嵌 Web、真实 systemd 启动、产品自启提交／冲突与服务启停；启动菜单48列真实 PTY、权限拒绝后管理员启用 linger、unit 写入后 enable 失败与显式恢复已通过；本轮未启动业务 Host 或创建数据库。SSH／整机启动、正式安装／卸载及完整 Desktop GUI 尚未验收。实测结果见[开发计划](../../docs/versions/v0.25.2/开发计划.md)。

## npm 分发（M6）

维护者发包操作见[Client npm 发布流程](../../docs/release/Client-npm发布流程.md)，包含构建、验收、next／latest、部分失败恢复与回退。

公开渠道目标安装方式（尚未公开发布）：

```sh
npm install -g @ez-assistant/client
# 升级或安装指定版本
npm install -g @ez-assistant/client@<版本>
# 先关闭自启并停止 Host，再移除 Client 与随包 Host（保留数据）
npm uninstall -g @ez-assistant/client
```

用户预装 Node `^22.12.0 || ^24.0.0` 与 npm。npm 主包含已编译 Client 和生产依赖，平台附件含编译 Host（内嵌 Web）；不携带 Node，不执行安装脚本或下载外部二进制。安装时不要省略 optionalDependencies，否则本机启动／配置会明确提示缺少平台包。

Host 直接使用 npm 平台附件 `node_modules/@ez-assistant/client-<平台>-<架构>/host/ez-assistant-runtime`，实际位置由 npm 解析。start、离线配置和新登记的 systemd 服务共用该来源，不复制到用户目录，无需指定额外的 Host 保留路径。进程与终端生命周期独立，关闭终端后 Host 可继续后台运行。

升级 Client 前，先停止对应 Runtime Home 的 Host，再运行 npm install；安装前缀或 Host 来源变化时，在 `config → 启动设置` 显式更新随包来源。原前缀不变时可直接 start。普通 restart 仍校验原实例来源，不在运行中自动替换程序。

卸载前在 `config → 启动设置` 关闭自启，并执行相同 Runtime Home 的 stop，再 npm uninstall。npm 会回收其管理的 Client 和 Host 文件，不自动停止进程或删除 systemd unit；共享数据及 Desktop 安装保留。不能依赖 npm 删除/替换可执行文件后在途 Host 继续正常工作。

旧私有候选已经复制出去的 Host 和原 unit 不自动删除或迁移。先停止原实例；若已登记自启，在启动设置中更新到当前随包来源后再启动。

本地候选包构建（输入为独立任务生成的 Host，输出目录必须不存在）：

```sh
node apps/client/scripts/pack-npm.mjs /绝对路径/新输出目录 \
  linux-arm64=/绝对路径/已构建的Host
```

脚本从版本源和锁文件重新编译、装配生产依赖，再执行 npm pack；支持 `darwin-arm64`、`linux-arm64`、`linux-x64` 输入。只把实际传入的平台加入主包可选依赖，候选报告恒标 `releaseReady: false`，不执行 publish。原生构建输入核实 build-info，跨架构输入先核对 ELF/Mach-O 架构，仍需在目标系统实测版本与行为。发布前另核验三方许可证、macOS Release／有效代码签名和实际 npm 安装、Linux x64 运行及 scope 权限；Developer ID／Apple 公证按渠道评估，不作为 npm 无条件前置。

隔离业务验收脚本 `tests/npm-distribution-smoke.mjs` 使用临时 registry 与 prefix，验证先停后重装、包内来源启动/重启、卸载删除程序及数据保留。路径定向验证 `tests/npm-payload-path.mjs` 直接解包并拦截 launch，检查来源、启动调用、自启预览及 `.local=775` 下不创建额外目录；不执行 npm 安装、业务 Host 或数据库操作。错误路径脚本 `tests/npm-package-failures.mjs` 覆盖附件缺失、清单摘要拒绝与 Node 版本错误。

### 私有仓库 Client 修订

npm 主包 `0.25.2-2` 改为直接使用随包 Host，仍精确依赖 `0.25.2` 平台附件；应用软件版本及兼容下限均为 `0.25.2`。此前 `0.25.2-1` 的外部保留目录方案已由本修订替代。

`0.25.2-3` 修复旧版 `/usr/bin/env` 不支持 `-S` 的入口问题，使用单参数 `#!/usr/bin/env node`。系统CA由HTTPS连接显式加载，保留默认/附加CA与证书校验，不依赖启动参数；Node版本和平台要求不变。

`0.25.2-4` 候选主包增加 `linux-x64` 附件依赖，平台附件仍为 `0.25.2`。x64 Host 以 glibc 2.17 为构建基线；这不等于整个 Client 支持 CentOS 7：Client 仍要求 Node `>=24.21.0 <25`，官方 Node 24 Linux 二进制要求 glibc >=2.28。旧系统上的 Node 22 仍会被入口拒绝，不能靠补充 Host 附件解决。候选发布及验证状态见[发布记录](../../docs/release/Client-npm发布流程.md#9-发布记录与当前缺口)。

`0.25.2-5` 将运行范围调整为 Node `^22.15.0 || ^24.0.0`，替代上述修订的 Node 24 限制。最低补丁线由 `tls.getCACertificates()` 决定：Node 22.13 仍需升级到至少 22.15；保留系统／默认／附加 CA 与证书校验，无需 `--use-system-ca`。Host 附件和应用兼容版本不变，Node 安装包自身的系统要求仍须满足。


后续修订 `0.25.2-7` 将上述下限继续降至 Node 22.12（22.x），仍支持 24.x。Node 22.12–22.14 使用默认 CA；私有 CA 通过 `NODE_EXTRA_CA_CERTS=/绝对路径/ca.pem` 指定。22.15+ 保留自动导入系统 CA，所有版本均严格校验证书。
