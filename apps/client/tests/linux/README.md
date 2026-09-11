# 本机 Docker Linux 包验收

## 17240 用户测试实例已替换（2026-09-10）

用户明确允许直接替换后，已将 service-m5-r2 的209文件完整包安装到原路径 `/work/ez-assistant-client-0.25.2-linux-arm64`，原命令可继续使用。操作前Host已停止；旧包保留为 `/work/ez-assistant-client-0.25.2-linux-arm64-previous-cx5x8r80`，不修改r2独立目录。

原 Runtime Home `/home/eztest/package-tests/ez-linux-package-utvrl_kj/home` 与配置保持，原路径新Host现已Ready，端口17240、自启disabled、密码和localhost配置均未变。Host运行摘要与新manifest一致；宿主 `http://127.0.0.1:17240/` 和 `http://localhost:17240/` 的HTML、入口JS均200。下文“17240仍旧包”均为历史记录，以本节为准。

切换前整个Home及独立SQLite Backup、切换后在线SQLite Backup均保留于 `/private/var/folders/94/0x59hjv55sq0vwc8mh2zcwr40000gn/T/ez-m5-replace-cx5x8r80`；全部42表共100行，前后计数和全字段摘要一致，完整性通过，应用ListSessions返回1条。未清空或恢复数据库，未接触本机生产环境。

测试对象是构建好的 Linux Client 包，包含编译后的 Client、生产依赖、固定 Node 24.21.0 和内嵌 Web 的 Release Host。运行容器不接收项目源码，不执行 npm install／npm build／cargo，不安装或运行 Desktop。

## 构建与运行分开

- `ez-assistant-m5-builder`：构建容器。原开发容器已改名保留，源码、Rust 工具链和旧夹具都留在此处。`Dockerfile` 和 `prepare-node.py` 用于准备该环境；Rust 安装在容器内，不修改宿主工具链。
- `ez-assistant-m5-linux`：干净运行容器，基于 `runtime.Dockerfile`。只有 Ubuntu 24.04、systemd、D-Bus、系统运行库及测试用户 `eztest`（UID 2000）；无系统 Node、npm、Rust、编译器或项目源码。
- 两者都使用本机 Docker Desktop 的 `desktop-linux` context。无宿主目录、Docker socket 或生产数据挂载，无端口发布；不使用远程服务器。

构建容器的 Host 源码位于 `/work/ez-assistant`；派生的 Cargo workspace 只保留 Host、crates 和解析所需的 debug-viewer 开发依赖，不构建 Desktop。原 manifest／lock 保留为 `Cargo.full-workspace.toml`／`Cargo.full-workspace.lock`。裁剪后的所有 registry 依赖版本和 checksum 必须逐项与原锁一致，然后用 `--locked` 构建。宿主仓库的 Cargo 文件不因此改动。

Web 使用现有独立 Web 构建入口 `npm run build --prefix apps/desktop`，仅把当次生成的静态产物复制到构建容器 `/work/web-dist`，通过 `EZ_ASSISTANT_WEB_DIST` 嵌入 Host；这不构建或运行 Desktop 程序。

```sh
# 在构建容器中，工具链已安装，源码和 Web 构建产物已备齐：
export PATH=/home/eztest/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export TMPDIR=/home/eztest/tmp
export EZ_ASSISTANT_WEB_DIST=/work/web-dist
cd /work/ez-assistant
cargo build --locked --release -p assistant-runtime-host -j 2
npm run build --prefix apps/client
```

`/tmp` 的 tmpfs 禁止执行，构建临时目录用容器内的 `/home/eztest/tmp`。当前实测 Rust 1.98.1、Linux arm64；其他架构不据此记为通过。

`assemble-package.mjs` 消费已编译 Client／Host、官方 Node 目录和独立安装的生产依赖目录，生成 M5 测试归档布局；生产依赖使用锁文件 `npm ci --omit=dev --ignore-scripts`，本地协议包仅保留编译输出。禁止整体打包开发 node_modules。入口清除 NODE_OPTIONS／NODE_PATH，直接执行随包 Node；manifest 保存逐文件 SHA-256，Node 许可证和第三方包许可证随包保留。

```sh
node /work/assemble-package.mjs \
  /work/ez-assistant /work/ez-assistant/target/release/ez-assistant-runtime \
  /usr/local /work/package-dependencies/apps/client/node_modules \
  /work/ez-assistant-client-0.25.2-linux-arm64
cd /work
tar -czf ez-assistant-client-0.25.2-linux-arm64.tar.gz ez-assistant-client-0.25.2-linux-arm64
sha256sum ez-assistant-client-0.25.2-linux-arm64.tar.gz
```

Node 官方归档由 `prepare-node.py` 下载并对照官方摘要；可选镜像仅代替归档下载，不能替代官方摘要。该测试归档尚不包含 M6 安装／卸载、跨架构和正式发布验收，不能标记为完整发布包。

## 创建干净运行容器

```sh
docker --context desktop-linux build -f apps/client/tests/linux/runtime.Dockerfile \
  -t ez-assistant-m5-runtime:0.25.2 apps/client/tests/linux
docker --context desktop-linux run -d --name ez-assistant-m5-linux \
  --label com.ez-assistant.purpose=m5-package-test --hostname ez-assistant-m5-linux \
  --privileged --cgroupns=private \
  --tmpfs /run --tmpfs /run/lock --tmpfs /tmp \
  --cpus 2 --memory 2g --pids-limit 1024 --stop-timeout 40 \
  ez-assistant-m5-runtime:0.25.2
```

先等待 `systemctl is-system-running` 返回 running，再执行 `systemctl start user@2000.service`。systemd PID 1 使用特权容器和私有 cgroup namespace；不挂载宿主 cgroup。原有容器不删除、不重建。

构建完成后先导出 tar.gz 并核对摘要，再复制到运行容器 `/work`；由 `eztest` 解压。只传归档，不传工作区或开发依赖。在宿主运行：

```sh
python3 apps/client/tests/linux/package-smoke.py \
  --package /work/ez-assistant-client-0.25.2-linux-arm64
```

驱动器通过 Docker exec 操作真实包，不往运行容器安装测试依赖。每次创建新的 Runtime Home，固定容器内端口 17240；发现端口冲突或校验失败即停止并保留现场。测试启动新 SQLite 后，逐表精确计数、字段摘要、完整性检查及独立备份回读；不连接生产库。

用例包含包摘要、无系统 Node、默认随包 Host 来源、帮助／版本、start／复用／restart／stop、内嵌 Web，以及真实 systemd enable／disable 与运行状态分离。服务用例直接注册产品 unit 模板；不把它冒充尚未接入 config 的完整自启产品流程。

容器重启用例先受控停止 Host 并核验数据库，再暂时移除 Client 入口及随包 Node 的执行权限，检查 systemd 直接启动 Host 的 UID、来源和 health。成功后恢复包权限、关闭测试 unit 自启并受控停止 Host，保留数据和备份。测试用户 linger 保留，不自动关闭用户级设置。容器重启不代表整台 Linux 服务器开机，A21 整机验收仍单独保留。

## 进入与复用

```sh
docker --context desktop-linux exec -it -u eztest \
  -e HOME=/home/eztest -e XDG_RUNTIME_DIR=/run/user/2000 \
  -e DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/2000/bus \
  -w /work ez-assistant-m5-linux bash
```

进入后使用包的 `bin/ez-assistant --runtime-home <专用测试目录> <命令>`，不使用 npm dev。停止容器前先受控停止业务 Host 并检查数据；Docker 超时可能强杀进程，不能以容器 stopped 代替数据库验收。不得自动删除保留中的测试目录。

## 本轮结果（2026-09-10）

Linux arm64 测试归档约 67 MiB，204 个文件摘要核验通过。SHA-256：`96bf17ad3ec74ef42ae489eca8d06f3fcddc8116fa71dedae39f60a7c4cfa082`。上述包与服务用例全部通过，测试库42表、基线13行、后续数据变化0，4份备份回读通过。运行容器内包位于 `/work/ez-assistant-client-0.25.2-linux-arm64`，测试 Home 为 `/home/eztest/package-tests/ez-linux-package-utvrl_kj/home`；Host已受控停止、unit自启已关闭，数据保留。

再次手动测试：

```sh
# 先按上文进入运行容器，再执行：
/work/ez-assistant-client-0.25.2-linux-arm64/bin/ez-assistant \
  --runtime-home /home/eztest/package-tests/ez-linux-package-utvrl_kj/home start
```

相同参数将 `start` 替换为 `status`／`stop`／`config` 即可。当前 HostControl 的服务 start/restart 适配尚未接入，不能把普通 start 当成通过 unit 启动；自动化已关闭此测试 unit 的自启。

## 宿主浏览器访问（2026-09-10 补充）

用户手测时要求增加映射，已创建独立 `ez-assistant-m5-web` TCP 转发容器，在 `ez-assistant-m5-test` 网络上连接运行容器。发布端口仅为 `127.0.0.1:17240`，浏览器访问 **http://localhost:17240/**。运行容器本身仍无源码、宿主挂载和 PortBindings；无需重建它。

转发配置见 `port-forward.nginx.conf`，NGINX 镜像固定为 `nginx:1.28-alpine@sha256:a8b39bd9cf0f83869a2162827a0caf6137ddf759d50a171451b335cecc87d236`，配置复制到转发容器 `/etc/nginx/nginx.conf`；不挂载 Docker socket。TCP 透传保留 HTTP／WS／SSE／TLS 协议，不改写凭据或访问头。Docker DNS 定期解析目标容器名，避免依赖固定容器 IP。NGINX 只是开发环境转发工具，不打入 Client 分发包。

Host 需开启非本地访问，并把 `localhost` 加入允许域名；后端看到真实连接来自转发容器，不能将其冒充本机可信连接。此次保留用户已开启的远程设置，通过 Host 在线 CAS 接口追加 `localhost` 并回读，Host 实例未变，无需重启。宿主访问 HTML 与入口 JS 均返回200。直接用 `127.0.0.1` URL 仍受后端非本地来源规则限制，请使用上面的 localhost 地址。

```sh
# 转发容器默认没有自动重启策略；需要时单独启动或停止。
docker --context desktop-linux start ez-assistant-m5-web
docker --context desktop-linux stop ez-assistant-m5-web
```

停止转发只关闭浏览器通道，不停止 Runtime Host。当前用户正在手测，Host 的运行状态以 `status` 为准，前文自动验收的“已停止”是验收结束时的历史状态。

## Client 自启提交与服务生命周期（2026-09-10 补充）

用户要求停止围绕 IP 输入小改扩大回归后，继续 M5 主线。IP／域名输入已统一接受合法 IPv4、IPv6、大小写及国际化域名；不再按 IP 类别排除普通访问。既有 17240 用户实例仍使用原包，本轮未替换它。

最新编译候选包：`/work/ez-assistant-client-0.25.2-linux-arm64-service-m5-r2`，宿主归档 `/tmp/ez-assistant-client-0.25.2-linux-arm64-service-m5-r2.tar.gz`，209 个文件摘要核验通过；SHA-256 为 `f4c6a8bd2b01be84601807ecfe8f6272b0872ea5578200a7988912b3d72603fc`。包含本轮地址输入修复、启动设置和 Client 服务启停，未重新构建 Rust 或 Web，也未运行 Desktop。

定向驱动 `service-smoke.mjs` 从 stdin 调用成品包的正式提交入口，真实 flock、systemd 和 Host；主链路在 `service-m5` 候选通过。r2 补充无 discovery 时等待服务残留收敛、start 等待 deactivating job 的边界，类型检查／15项相关测试通过，并在容器验证已停止服务的幂等 stop；未重新跑整条链路或全量回归。

使用独立 Home `/home/eztest/ip-fix-HVzux5/home` 和端口17241。已通过开启不启动、关闭不停机、真实提交锁竞争、过期预览拒绝、活动来源保护、disabled 服务沿原 unit 重启及受控停止。最终 unit `ez-assistant-fe691a78f88365e6e8142cc681378f52c2d57d47355972414471a6ccfe371549.service` 为 disabled／inactive，登记来源仍是原 `service-m5` 候选，r2 不自动重绑已登记来源。测试库42表、13行，前后全部计数与字段摘要一致；两份独立备份回读通过，证据 `/tmp/ez-v0252-m5-service-evidence`。

进入容器后体验最新 Client 的启动菜单（所有动作均针对17241独立 Home）：

```sh
/work/ez-assistant-client-0.25.2-linux-arm64-service-m5-r2/bin/ez-assistant \
  --runtime-home /home/eztest/ip-fix-HVzux5/home config
```

`启动设置` 支持独立预览／保存；需要换到新包 Host 时显式选择切换来源。此处无需改动17240实例。M5 的 Client 菜单 PTY、首次设置权限拒绝／管理员开启 linger、unit 写入后 enable 失败及显式恢复已完成定向验证；整机开机仍未验证。Desktop 自启逻辑独立，按用户纠正不新增服务器 systemd 适配。

### 启动设置定向验收

使用同一干净容器中的专用普通用户 `ezservicecheck`（UID2001），与用户手测的 `eztest`（UID2000）分开。未启动业务 Host、未创建 SQLite。成品包仍为上述 r2，无产品代码或包内容改动。

- 首次设置：只启动2001的用户管理器，未开启 linger；Client 提交在 loginctl 步骤失败，显示管理员命令。回读未注册，Runtime Home 尚不存在。管理员显式开启该测试用户 linger 后，新预览可保存；未让 Client 使用 root 或获取 sudo 密码。
- 部分失败：只把2001的 `default.target.wants` 临时设为0500；真实 unit 写入及 daemon-reload 成功，enable 因权限失败。Client 显示已完成步骤及 disabled／inactive，不自动回滚或继续操作。核验预期故障后恢复该夹具目录为0700，重新预览提交成功；关闭自启仍保留 linger。
- 真实 PTY：`startup-terminal-smoke.py` 经 Docker exec 操作包入口，48列终端通过预览取消、开启保存、关闭保存后 Ctrl+C 三项；取消未提交预览不创建 Home，已保存设置不因取消撤销，所有 unit 均未启动 Host。

PTY 记录：`/private/var/folders/94/0x59hjv55sq0vwc8mh2zcwr40000gn/T/ez-m5-startup-pty-q6gc2mm3`；最终只读状态：`/tmp/ez-v0252-m5-service-settings-final.json`。两份测试 unit 均 disabled／inactive；随后仅撤销本轮新建测试用户2001的 linger 并停止其用户管理器，保留账户、配置与记录。2000的 linger、服务、17240实例及转发均保持原状。

复测 PTY 前需由测试环境管理员启动2001的用户管理器并开启其 linger，再从宿主执行：

```sh
python3 apps/client/tests/linux/startup-terminal-smoke.py \
  --package /work/ez-assistant-client-0.25.2-linux-arm64-service-m5-r2
```

M5 当前按已授权 Docker 范围待确认。A21 的真实 Linux 整机重启／无需交互登录尚未验证，不能将现有容器启动证据标为该项通过。

## M6 npm 候选包验收

M6 以 npm 为主要渠道。运行容器仅接收 `pack-npm.mjs` 输出的 tgz 与官方 Node/npm 工具链；测试驱动通过 stdin 执行，不拷入项目源码。Node/npm 仅解压在 `/home/eztest/m6-npm/toolchain`，不修改系统 PATH，也不替换现有 `/work/ez-assistant-client-0.25.2-linux-arm64` 或17240测试实例。

最终候选目录 `/home/eztest/m6-npm/packages-final`，包含主包、Linux arm64和macOS arm64附件（npm在Linux仅安装适配附件）；尚未发布至公网 registry。测试入口使用短时回环 registry 接受这些包，无外部二进制下载脚本：

```sh
docker --context desktop-linux exec -i -u eztest \
  -e HOME=/home/eztest -e TMPDIR=/home/eztest/m6-npm \
  -e XDG_RUNTIME_DIR=/run/user/2000 \
  -e DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/2000/bus \
  ez-assistant-m5-linux /home/eztest/m6-npm/toolchain/bin/node \
  --input-type=module - /home/eztest/m6-npm/packages-final \
  < apps/client/tests/npm-distribution-smoke.mjs
```

每次生成新的隔离 prefix、Host保留目录和Runtime Home，打印证据路径；结束时停止Host、关闭测试unit自启，保留数据库、在线备份和固定程序。该入口是安装生命周期定向测试，不代表Linux整机开机、Linux x64或正式公开发布通过。
