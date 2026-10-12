# Docker Compose 部署

## 运行结构

- `frontend`：构建 React 前端，由 Nginx 提供页面；`/api/` 同源反向代理到 Rust。
- `backend`：一个进程同时运行 HTTP API 和 MongoDB 持久化后台执行器，不依赖 RabbitMQ、Celery 或 Redis。
- `mongo`：带认证的单节点副本集，支持资源绑定、角色变更和回收恢复所需事务；不映射宿主机端口。
- `mongo-key` / `mongo-init`：一次性生成副本集密钥、初始化副本集和仅有应用数据库读写权限的用户。
- `mailpit`：本地验证邮件接收器，Web 界面只绑定 `127.0.0.1`；不向真实用户发送邮件。

基础前提：Docker Engine/Desktop 可用，包含 Docker Compose v2 与 BuildKit；镜像及 Rust/npm 包仓库可访问。真实上传、导出及图像处理需要私人 COS bucket；AI 需要支持图像输入的 OpenAI 兼容模型服务。Compose 不自动创建腾讯云账号、bucket 或模型账号。

## 一键启动

从仓库根目录运行。第一次脚本会创建 `backend-rust/.env`，生成独立随机的 MongoDB root 密码、应用用户密码、JWT 密钥和管理员密码。填写云配置后重跑同一条命令即可部署；已有 `.env` 不会被覆盖。

Windows PowerShell：

```powershell
powershell -ExecutionPolicy Bypass -File .\backend-rust\deploy.ps1
```

Linux/macOS（需要 `openssl` 生成首次密钥）：

```bash
bash backend-rust/deploy.sh
```

如果仅检查账户与项目等元数据功能，可显式允许未配置云：

```powershell
powershell -ExecutionPolicy Bypass -File .\backend-rust\deploy.ps1 -AllowUnconfiguredCloud
```

```bash
bash backend-rust/deploy.sh --allow-unconfigured-cloud
```

此模式不能上传或导出文件，也不是完整云功能部署。没有模型配置时，AI 入口返回配置错误，不产生假结果。

已有完整 `.env` 时也可以直接运行一条 Compose 命令：

```bash
docker compose --env-file backend-rust/.env -f backend-rust/compose.yaml up -d --build --wait --wait-timeout 180
```

默认访问：

- 前端：`http://localhost:8080`。
- 本地验证邮件：`http://localhost:8025`。
- 后端健康检查：`http://localhost:8080/api/health`。
- 管理员邮箱及密码：查看自己的 `.env` 中 `ADMIN_EMAIL`、`ADMIN_PASSWORD`，不要将密码贴入日志或提交到仓库。

管理员只在账号不存在时创建；重启不会重置密码。修改 `.env` 也不会自动修改已经初始化的 MongoDB 用户密码，需走明确的凭证轮换流程。

## 必填与可选配置

完整配置模板见 `.env.example`。`.env` 已被 Git 和 Docker 构建上下文忽略。

### 腾讯云 COS

```dotenv
COS_REGION=ap-shanghai
COS_BUCKET=your-bucket-1250000000
COS_SECRET_ID=...
COS_SECRET_KEY=...
```

bucket 必须私有，账号仅授予该资源空间所需的对象读写、条件复制、删除及分块操作权限；不得开启匿名写入。`COS_ENDPOINT` 通常留空，默认使用 bucket COS 域名。覆盖值是 bucket origin，并仍使用腾讯 COS 签名，不是 AWS S3 签名或本地文件存储回退。

资源 key 使用应用生成的暂存及正式命名空间。客户端只有暂存上传授权；验证后通过 COS 条件复制到正式唯一 key，客户端没有正式 key 写权限。正式资源替换更新引用，不覆盖对象。

浏览器需要 COS CORS 配置：

- 允许真实前端 origin；本地为 `http://localhost:8080`，正式环境换成 HTTPS 域名。
- 允许直传 `PUT`、资源读取 `GET` 和必要的 `HEAD`；浏览器预检由 COS 处理。
- 允许 `Content-Type` 及签名上传所需请求头；签名按精确请求头计算，不要在上传时修改。
- 暴露 `ETag`、`Content-Length`、`Content-Type`，便于客户端与服务验证。
- 分块上传走同样的 bucket origin 和短期签名；分块初始化、合并、中止及确认由后端执行。

配置 COS 未完成分块生命周期作为失联初始化/断点续传遗留碎片的兜底。生命周期仅命中暂存空间，不能按上传年龄删除正式业务资源。

### SMTP

默认 `SMTP_HOST=mailpit`、端口 `1025`、`SMTP_TLS=none`，验证码发送到本地 Mailpit，适合部署验证。公网生产环境必须改为真实 SMTP：

```dotenv
SMTP_HOST=smtp.example.com
SMTP_PORT=587
SMTP_TLS=starttls
SMTP_USERNAME=...
SMTP_PASSWORD=...
SMTP_FROM="MoeFlow <noreply@example.com>"
```

`SMTP_TLS` 支持 `none`、`starttls`、`tls`；通常 587 用 `starttls`、465 用 `tls`。禁用 TLS 只适合可信本地接收器。真实 SMTP 配置后 Mailpit 可以停用，不影响外部发送。

### AI

```dotenv
AI_BASE_URL=https://your-provider.example/v1
AI_API_KEY=...
AI_MODEL=your-vision-model
```

模型必须接受图像并返回项目要求的结构化文字、位置和翻译结果；供应商是否支持相应参数需要实际验证。凭证仅在后端使用，不通过前端配置或任务响应返回。前端提交持久化任务并查询结果，关闭页面不应丢失任务。

### 容量与并发

初始配置：用户容量 1 GiB、团队容量 100 GiB；`USER_STORAGE_QUOTA_BYTES`、`TEAM_STORAGE_QUOTA_BYTES` 调整后在后续资源预占时应用，不删除已有资源。`MAX_UPLOAD_BYTES` 是通用申请上限，具体用途另有限制：头像 10 MiB、图片 32 MiB、UTF-8 文本/LabelPlus 16 MiB、压缩导入包 512 MiB。ZIP 展开总量不超过 512 MiB，并校验路径、实际大小及内部图片/文本。文本每文件最多 100000 个非空行。

`MAX_ACTIVE_UPLOADS_PER_USER` 默认 8、`MAX_ACTIVE_UPLOADS_PER_TEAM` 默认 32，限制未完成的客户端上传预占；并发申请通过 MongoDB 用户/容量账户事务串行化。上传完成释放并发槽位；取消或失联会话仍需考虑已签发授权的失效保护窗口。容量直到物理删除成功才释放。

单次导出预算由 `MAX_EXPORT_INPUT_BYTES`（默认 512 MiB，含 ZIP 元数据）、`MAX_EXPORT_TEXT_BYTES`（默认 64 MiB）及 `MAX_EXPORT_SOURCES`（默认 100000）限制。超限明确失败，不截断；使用文件选择缩小导出范围。模型响应实际读取上限 8 MiB。

`JOB_CONCURRENCY` 默认 4，`JOB_HEAVY_CONCURRENCY` 默认 1；重任务仅在有重任务名额时领取，保留轻任务名额给邮件和回收。根据实例内存、CPU 和外部服务限额调整。API 和执行器同进程，进程级 OOM 或崩溃仍会影响 API。以上是初始安全配置，不是性能测量结论。

### 回收及恢复保护

- `GC_ENABLED=true`：开启资源回收；到期未绑定、临时对象和导出可回收。
- `RESOURCE_PROTECTION_ENABLED=true`：启用业务资源删除保护政策。
- `RESOURCE_BACKUP_CONFIRMED=false`：默认不允许永久删除可恢复业务对象的资源。

只有数据库和对象联合恢复方案已部署、演练并满足恢复窗口后，才设置 `RESOURCE_BACKUP_CONFIRMED=true`。这一变量是运维确认，不会自动创建备份；不能用一个布尔值代替备份。

项目完结只读，不删除源文件。业务删除保留 7 天恢复期；到期后开始永久回收，失败重试。旧文件替换先提交新引用，再延迟回收旧资源。未绑定上传保留 24 小时、上传会话最长 24 小时、普通签名 15 分钟、导出 7 天、下载签名 5 分钟。短期 URL 不保证严格即时撤销，也无法收回已下载内容。

备份至少覆盖 MongoDB 与相关 COS 对象；数据库快照不能独自恢复已物理删除的文件。灾难恢复时先停任务领取与 GC，恢复后对账资源、任务状态和外部操作，再继续执行，不能未经核对重放删除任务或邮件。

## 公网部署

默认 HTTP 端口只绑定 `127.0.0.1`。公网使用反向代理及有效 HTTPS 证书；根据入口修改 `HTTP_BIND`、`HTTP_PORT`、`CORS_ORIGINS` 和 COS CORS。不要把 MongoDB、Mailpit 界面或模型内部接口暴露公网。

首次初始化数据库需要保留 `mongo-data`、`mongo-key` 卷。下述停止命令不删除数据；不要随意使用 `down -v`。

```bash
docker compose --env-file backend-rust/.env -f backend-rust/compose.yaml logs --follow backend
docker compose --env-file backend-rust/.env -f backend-rust/compose.yaml ps
docker compose --env-file backend-rust/.env -f backend-rust/compose.yaml down
```

日志不应包含 COS 密钥、模型 API key、完整预签名 URL 或验证码。密钥变更需要同步对应服务及凭证撤销；JWT 密钥变更会使现有令牌失效。

## 镜像仓库无法访问时

模板支持 `RUST_IMAGE`、`RUNTIME_IMAGE`、`NODE_IMAGE`、`NGINX_IMAGE`、`MONGO_IMAGE` 和 `MAILPIT_IMAGE` 覆盖。Docker Hub 不可达时可以选择可信镜像源，例如 Docker 官方库的 `public.ecr.aws/docker/library/...`。不要关闭 TLS 校验绕过拉取失败。

## 验证范围

实际验证记录见 [README.md 第 11 节](README.md#11-参考与验证限制)。已运行 Compose 完整构建和启动、生产前端浏览器登录/编辑/并发直传/失败重试/导出/只读，以及持久化进程恢复和资源清理。COS 与模型部分使用隔离协议夹具，签名另与官方 SDK 对比；真实桶 CORS/IAM、供应商质量和生产备份恢复未因这些检查而得到验证，仍需对应凭证与实际环境联调。
