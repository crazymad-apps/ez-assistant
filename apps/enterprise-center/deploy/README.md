# 企业中心 Docker 部署

一个对外端口同时提供官网 `/`、管理后台 `/admin/` 与后端 `/api/`。Nginx 入口负责静态页面和
请求分流，Center 仅在容器网络监听；PostgreSQL 独立持久化。默认 HTTP，需要 HTTPS 时由现有
外层反向代理终止 TLS，并把 `CENTER_PUBLIC_ORIGIN` 设置为用户与 Host 实际访问的 origin。

## 成套构建

在两个工程安装构建依赖后，在 `apps/enterprise-center` 执行：

```bash
npm run package:docker
# 以下两个镜像必须使用上一步同一个 directory 和同一个唯一发布标签。
docker build -f <directory>/Dockerfile -t ez-enterprise-center:<标签> <directory>
docker build -f <directory>/Dockerfile.entry -t ez-enterprise-entry:<标签> <directory>
```

构建与生产依赖安装均在 Docker 外完成。装配先校验官网、后台的入口、清单和全部资源，再交付
`dist`、`public/admin`、`public/site`、生产依赖和部署配置。两个镜像只复制同一装配产物，不在镜像
中运行 npm 或编译器。Center 以 Node 24 非 root 用户运行，入口以 nginx 用户运行；公开内容不编入镜像。
脚本拒绝未经目标平台核对的原生生产依赖。传输地址、账号与专用脚本留在忽略的 `.local/` 中。

## 首次部署

将制品中的 `compose.yaml` 复制到独立部署目录，并创建权限为 600 的 `center.env`：

```dotenv
CENTER_DATABASE_URL=postgresql://center_app:replace-me@database.example.com:5432/ez_enterprise_center
CENTER_PUBLIC_ORIGIN=https://center.example.com
```

在同目录 `.env` 设置：

```dotenv
CENTER_IMAGE=ez-enterprise-center:<标签>
CENTER_ENTRY_IMAGE=ez-enterprise-entry:<相同标签>
CENTER_DOCKER_NETWORK=<可以访问专用数据库的既有网络>
CENTER_BIND_ADDRESS=0.0.0.0
CENTER_PUBLISHED_PORT=7320
```

准备挂载目录：

| 路径 | 内容与权限 |
| --- | --- |
| `data/snapshots` | 调用证据私有目录，Center UID/GID 1000:1000 可写 |
| `data/plugins` | 插件私有目录，1000:1000、0700，不允许软链 |
| `data/desktop-releases` | 安装包私有目录，1000:1000、0700，不允许软链 |
| `data/site-content` | 专用公开内容目录，目录 0755、文件 0644；入口只读挂载，禁止放入私有文件 |
| `config/model-templates.json` | 从同批 Center 镜像的 `dist/resources/model-templates.json` 初次复制，只读挂载 |

公开目录可以先为空，页面会提示内容尚未配置。按[内容维护说明](../../../docs/site/README.md)
将仓库配置与 Markdown 校验后发布到 `data/site-content`；后续更新无需重建镜像或重启。
必须挂载目录，不能单独挂载 `content.json`，否则宿主原子替换文件后容器可能继续读取旧 inode。
保留旧哈希图片，已有标签页继续使用其原正文对应的图片。回退内容时重新发布经核对的仓库版本。

仅入口映射宿主端口，Center 没有 `ports` 或固定容器名。入口不挂载数据库、快照、插件、安装包
和模型配置；不要额外把 Center 端口暴露出去。Compose 禁止自动建库／迁移，不能用 API-only
绕过完整静态制品检查。默认安装包验证公钥随 Center 制品交付；如需配置另一发布公钥，仅挂载公钥
并设置 `CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE`，不得把签名私钥放入服务器。

先核对数据库主机、端口、库名、账号、实际表及预期影响，按仓库数据库规则取得批准并准备必要备份。
新库由运维独立创建，不使用其他业务库。使用应用专用账号；迁移通过同一个账号与唯一迁移账本执行。

```bash
docker compose config --quiet
# 数据库目标核验与授权完成后才执行升级。
docker compose run --rm --no-deps center node dist/database/cli.js upgrade
docker compose run --rm --no-deps center node dist/database/cli.js check
docker compose run --rm --no-deps entry -t
docker compose up -d
curl --fail http://127.0.0.1:7320/
curl --fail http://127.0.0.1:7320/admin/
curl --fail http://127.0.0.1:7320/api/info
```

核对容器健康、数据库实际表与关键字段、静态正文以及真实下载。后台初始账号为 `admin / 123456`，
首次登录后修改密码。进程重启会使内存登录 Token 失效，需要重新登录。

## 路由和传输边界

官网仅开放 `/`、`/download`、`/changelog`、`/manual` 和合法单段文章路径；后台保留 Hash 路由。
未知 API 返回后端错误，未知资源／其他路径返回 404，不回退首页。点文件、源码映射、构建清单、
目录列表和软链禁止读取。正文 JSON 每次重新验证，HTML 不持久缓存，哈希资源可长期缓存。

API 保留原 URI、方法、Authorization、状态码及流式响应，不重试请求、不缓冲上传／下载／模型流。
入口正文上限为 2 GiB，兼容当前插件与双 512 MiB 安装文件；具体接口仍执行自己的业务限额。
收发空闲超时为一小时，Center 仍执行已配置的业务空闲期限；没有新增模型任务总时限。
断开下游会传给 Center，Upgrade 头仅条件转发，不因此增加应用级 WS 接口。

Center 停止时官网外壳、已发布静态手册与后台外壳仍可访问，动态安装包／日志／后台操作显示失败。
Center 重建后入口通过 Docker DNS 重新解析地址，无需因容器 IP 变化手动重启入口。

## 升级、备份与回退

保留旧镜像与部署配置，核对待执行迁移和内容格式。成套替换 `CENTER_IMAGE` 与 `CENTER_ENTRY_IMAGE`，
两者必须来自同一装配结果；不要使用可变 latest 标签拼接不同构建。更新后重新核对页面、API、下载和
流式调用。真实部署与用户数据操作另行授权，开发验收不代表线上升级。

旧单容器部署升级前，核对安装包实际存储位置；若曾保存在容器可写层，须先停写并独立备份、验证文件
与表记录，再按授权迁移到持久目录，不能直接替换容器导致包丢失。旧 `container_name` 与端口映射在切换
时也要核对，避免旧容器仍占端口；不要盲目删除旧容器。

备份覆盖 PostgreSQL、私有安装包／插件／快照文件、模型配置、公开内容目录及部署配置。通过实际
恢复与表计数、文件大小／SHA、接口核对确认可恢复。仅没有数据库格式变化时可直接成套回切旧镜像；
已迁移数据库须恢复匹配的程序、数据库与文件，不能只回退镜像。公开内容独立回退且保留旧图片；不清空
数据卷，不因官网更新自动删除历史安装包或调用证据。
