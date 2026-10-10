# 企业中心 Docker 部署

前后端共用一个 Node 24 进程和 7320 端口：管理入口 `/admin/`，后端 `/api/`。
默认支持 HTTP；需要 HTTPS 时由现有反向代理终止 TLS，并同步修改 `CENTER_PUBLIC_ORIGIN`。
PostgreSQL 16 独立持久化，不塞入应用容器。

## 构建和上传

先为两个工程安装构建依赖，在 `apps/enterprise-center` 执行：

```bash
npm run package:docker
docker build -t ez-enterprise-center:<唯一镜像标签> <末尾输出的directory>
```

构建、生产依赖安装均在 Docker 外完成。制品包含 `dist`、`public/admin`、`node_modules`；
镜像只复制制品，不运行 npm 或编译器。当前生产依赖为跨平台 JavaScript；装配脚本遇到原生
依赖时拒绝打包，届时须在目标平台准备依赖，不能复制 macOS 原生模块到 Linux。
镜像使用服务端原生架构，非 root 用户运行。将装配制品或镜像传到自行选择的运行环境；
具体主机、传输脚本和凭据属于本地运维资料，放在忽略的 `.local/` 中，不随源码分发。

## 首次部署

选择独立部署目录，将 `compose.yaml` 复制到其中，创建权限为 600 的 `center.env`：

```dotenv
CENTER_DATABASE_URL=postgresql://center_app:replace-me@database.example.com:5432/ez_enterprise_center
CENTER_PUBLIC_ORIGIN=https://center.example.com
```

将所选镜像的 `dist/resources/model-templates.json` 首次复制到 `config/model-templates.json`。
创建 `data/snapshots` 和 `data/plugins`，归属容器的 UID/GID 1000:1000；`data/plugins` 权限为
0700，不得是符号链接，也不得放在公开静态目录内。升级不覆盖这些外置文件；备份与恢复同时覆盖
PostgreSQL 和 `data/plugins`，按当前包记录逐行核对文件长度与 SHA。
显式设置 `CENTER_DOCKER_NETWORK`，选择能够访问 PostgreSQL 的既有 Docker 网络。

先核对数据库目标、账号和表，并按仓库数据库规则完成批准及必要备份。新库由运维独立创建，
不使用其他业务库。使用本应用专用数据库账号，实际密码只存私有配置。应用升级入口为
`node dist/database/cli.js upgrade`，不是 ORM 自动建表。
确认后使用所选镜像一次性执行升级，再用 `check` 核对版本和逐表计数。
Compose 固定关闭自动升级，防止每次替换镜像意外修改数据库。

```bash
export CENTER_IMAGE=ez-enterprise-center:<所选标签>
docker compose run --rm --no-deps center node dist/database/cli.js check
docker compose up -d
curl --fail http://127.0.0.1:7320/api/info
curl --fail http://127.0.0.1:7320/admin/
```

将 `CENTER_IMAGE` 和 `CENTER_DOCKER_NETWORK` 保存到部署目录 `.env` 供后续 Compose 使用。
后台初始账号为 `admin / 123456`；首次登录后修改密码。
对外服务通过反向代理启用 HTTPS，并同步设置公开访问地址。

## 更新与回退

使用新唯一标签上传并构建，保留旧镜像与版本目录；核对是否有待执行数据库迁移。
涉及既有库迁移时先停写、备份并恢复核验，取得授权后再执行。
更新 `.env` 的镜像标签并 `docker compose up -d`，检查容器健康和同源管理入口。
仅没有数据库版本变更时可直接切回旧镜像；已升级数据库不能用镜像回退替代数据恢复。
进程重启会使内存登录 Token 失效，用户需要重新登录。
