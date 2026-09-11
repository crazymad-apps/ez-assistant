# client 模块约束

正式 `apps/client` 是与 Desktop 同级的本机 Host 管理产品。使用 TypeScript、Node.js、Commander、Clack、Ora 和 Picocolors；不用 Bun、全屏 TUI 或原生 Node 扩展。

- 改动遵守开发流程规范、前端编程规范中适用的 TypeScript／异步清理／中文注释约束，以及 v0.25.2 技术方案与交互指导。DOM、React 和 Sass 规则不适用。
- 普通命令为 start／status／stop／restart／web；config 是唯一交互入口，访问设置与启动设置独立预览、提交。M5 已接入 Client 自启注册和服务生命周期；本机 Docker Linux 仅测试 Client／Host／自启，不运行 Desktop；不管理 remote、Agent 或 Session。
- 共享 DTO 与版本判定消费 @ez-assistant/protocol，不手写业务协议副本。Client 只持有草稿和短生命周期连接，不持有业务权威状态。
- Runtime Home 显式参数 > 环境变量 > 产品默认；发现、原生凭据、来源、锁和配置由 Host 拥有。禁止直接写 TOML、访问数据库、实现密码哈希或建立另一份实例账本。
- stop 固定原地址／实例／凭据；restart 验证原来源，不能回退到调用方随包 Host。超时或取消不重放写请求、不强杀 Host。
- 离线 config 使用 Host 有界 JSON IPC；提交后取消等待现有操作收敛，不强杀 helper，不假称回滚。普通输出不得包含 token、密码、哈希或证书内容。
- 进入 config、status 与预览不创建 Home 或修复权限。发现或认证无法核实时不得当作离线。
- 测试统一放 apps/client/tests，源码不导入测试夹具。真实验证始终使用临时 Home，不接触生产数据或安装版 GUI。
- Client 构建与内部框架可独立替换；Host、Desktop、共享协议不得依赖 Client 私有代码或随包 Node。

- systemd 状态查询不依赖 Host 业务就绪或 Client 私有安装布局。unit 读取失败、drop-in、来源或加载状态变化都保留未知／冲突；禁用和未运行分开，不把文件存在视为开机自启成功。
- 自启提交通过 flock 持有短锁（普通用户运行目录，root 使用 /run/ez-assistant-client）；内部 Node 提交进程不属于长期服务，终端取消不向它转发信号。失败只回读，不继续补救或自动撤销 linger；关闭自启无需调用 Host 兼容检查。
- 启动采用已启用的原生 unit；服务实例重启即使 disabled 仍沿原 unit。受控停止只向固定原实例发送应用命令，随后核实 unit 与 cgroup 已收敛，不补发 systemctl stop 或强杀。

- npm 是主要分发渠道。Host 直接位于 node_modules 平台附件中；start、helper 和新登记的 systemd ExecStart 使用随包来源，不建立外部保留副本。Host 进程独立于 Client/Node，但程序文件随 npm 升级或卸载；升级前显式停止，卸载前关闭自启并停止，不依赖 postinstall/uninstall 钩子。npm 不自动改写 unit、清理历史候选副本或删除共享数据；已有实例/注册来源不自动切换。

- 普通手动启停不要求 systemd 用户管理器在线。管理器不可用时，仅在固定名称的 Client 自启注册经读取确认不存在时允许手动管理；已有、不安全或不可读注册仍拒绝，状态查询保留未知，不伪装自启关闭。

- root 配置 Linux 自启使用系统级 systemd（/etc/systemd/system、User=0、multi-user.target）；普通用户保留用户级及 linger，默认均关闭。不提权、不迁移 Home、不自动覆盖另一范围的同名注册。查询与启停使用同一实际范围；系统级不执行 loginctl。
