# 架构说明

业务流量默认走 iroh（QUIC，端到端加密）；本应用不运营内容服务器。连接凭证可以是 `EndpointTicket` 分享码，也可以通过广场 gossip 发现。

1. 发布端选择文件夹后，`server::StaticHost` 用 axum 监听 `127.0.0.1:0`。每个请求先经过 `security::resolve_within_root`：只解码一次，拒绝 `..`、隐藏文件、NUL 和逃出根目录的符号链接，再读文件。
2. `p2p::Node` 用持久化的 Ed25519 密钥启动 iroh endpoint。`StallHandler` 接受 ALPN `popstall/http/1` 的双向流，把路径交给同一个 axum router，再把状态码、内容类型和正文写回。
3. 访问端解析分享码或广场记录并 `connect`。`GuestProxy` 只绑定 `127.0.0.1:0`，把浏览器的 GET/HEAD 收成一帧。它不转发原始 Host 以外的请求头，也不接受指向其他地址的 Host。
4. 访客窗口加载这个代理地址。窗口标签是 `guest-*`，不在 `capabilities/default.json` 里，因此没有 Tauri 命令权限。导航被限制在该代理源，`window.open` 被拒绝。代理响应带上只允许 `'self'` 的 CSP。

## 路径与信任

很多访客位于多层局域网、NAT 或 CGNAT 后面。节点优先使用直连和 iroh relay；两者不可达时才会把 helper 当作升级路径。relay-only helper 可以只携带 relay URL 参与发现，但只有与本地 endpoint 共享可达 relay 时才被视为可用；端口映射默认关闭，显式开启后只是尽力改善直连成功率，失败仍回退 relay。helper 只负责转发，它不可信：

- 直连和 iroh relay 路径的 QUIC 已经端到端加密，应用层不重复加密。
- helper 桥接路径用一次性 X25519 会话密钥和 AES-GCM 封包，密钥协商由双方持久化 Ed25519 身份签名并绑定会话 id、guest/host/helper 身份。helper 只能看到密文和必要元数据，看不到请求正文或响应正文。
- UI 状态明确区分直连、relay 和 helper 路径；helper 不会被显示成端到端直连。

## 内容完整性与恢复

发布端按 `(路径, 长度, 修改时间)` 缓存文件的 BLAKE3 全文摘要，并在响应头证明中交给访问端。访问端在写入缓存和播放前校验摘要；影音切片还会校验 `Content-Range` 与缓存片段的一致性。摘要不匹配时丢弃已缓存的坏字节，沿 `Range` 重试一次，再失败则报错，不把损坏内容交给页面。

`Engine::App`（打包进客户端的静态页面）没有可校验的文件根，因此不携带伪造摘要；只有用户选择的本地文件夹才启用完整性证明。分块 Merkle 证明是后续增强，不改变现有的整文件摘要。

## 发现与降级

广场通过 gossip 交换摊位和 helper 记录。pkarr HTTP 公告保留，因为它对 NAT 友好，但发布密钥不再硬编码在源码中：公钥是固定发现锚点，发布密钥来自持久化节点身份或用户配置，缺少发布权限时自动降级为只读。发现过程会尝试多个 relay URL，并保留最近成功的 bootstrap 节点；外部 DHT、relay 或路由器不可用时只影响可发现性和建连速度，不破坏已建立会话的加密与完整性保证。

所有连接尝试都有超时和有限退避；helper 升级耗时、重试次数、摘要失败和路径类型都会计入网络指标，方便确认多层 NAT 下的实际行为。

身份文件在应用数据目录的 `endpoint.key`。打烊会清掉托管目标并关掉本地 axum；P2P 节点留到进程退出，方便接下来继续串门。
