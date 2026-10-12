# moeflow

全新 Rust 后端与已适配前端：

- [`backend-rust/README.md`](backend-rust/README.md)：架构、资源生命周期与验收记录。
- [`backend-rust/DEPLOYMENT.md`](backend-rust/DEPLOYMENT.md)：Docker Compose 部署、COS/模型/SMTP 配置及恢复边界。
- `frontend-v1/`：当前前端代码，文件直接上传 COS，后台计算由 Rust 服务持久化调度。
- `backend-v1/`：原项目参考代码，不参与新部署，不迁移旧数据或兼容旧客户端。

Windows：

```powershell
powershell -ExecutionPolicy Bypass -File .\backend-rust\deploy.ps1
```

Linux/macOS：

```bash
bash backend-rust/deploy.sh
```

首次运行生成本地随机凭证配置；按部署文档填写真实 COS 和模型配置后，用同一命令启动。仅测试账户/项目元数据时可显式使用 `-AllowUnconfiguredCloud` 或 `--allow-unconfigured-cloud`。生产使用 HTTPS、真实 SMTP，并在确认数据库与对象恢复保护后启用业务物理回收。
