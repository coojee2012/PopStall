# 架构说明（Phase 1）

业务流量走 iroh（QUIC，端到端加密）。本阶段没有广场广播，连接凭证是一份 `EndpointTicket` 分享码。

1. 发布端选择文件夹后，`server::StaticHost` 用 axum 监听 `127.0.0.1:0`。每个请求先经过 `security::resolve_within_root`：只解码一次，拒绝 `..`、隐藏文件、NUL 和逃出根目录的符号链接，再读文件。
2. `p2p::Node` 用持久化的 Ed25519 密钥启动 iroh endpoint。`StallHandler` 接受 ALPN `popstall/http/1` 的双向流，把路径交给同一个 axum router，再把状态码、内容类型和正文写回。
3. 访问端解析分享码并 `connect`。`GuestProxy` 只绑定 `127.0.0.1:0`，把浏览器的 GET/HEAD 收成一帧。它不转发原始 Host 以外的请求头，也不接受指向其他地址的 Host。
4. 访客窗口加载这个代理地址。窗口标签是 `guest-*`，不在 `capabilities/default.json` 里，因此没有 Tauri 命令权限。导航被限制在该代理源，`window.open` 被拒绝。代理响应带上只允许 `'self'` 的 CSP。

身份文件在应用数据目录的 `endpoint.key`。打烊会清掉托管目标并关掉本地 axum；P2P 节点留到进程退出，方便接下来继续串门。
