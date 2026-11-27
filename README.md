# 闪摊 PopStall

闪摊是跨平台桌面应用：把一个本地文件夹开成摊位，生成 iroh 分享码；另一台客户端粘贴分享码或通过广场发现后，在没有 Tauri 权限的窗口里打开对方页面。应用不依赖闪摊自己的内容服务器。

## 这是桌面应用

闪摊运行在自己的窗口里，覆盖 macOS、Windows 和 Linux。不要在浏览器打开开发地址，那里没有系统权限，选择文件夹会失败。

开发时先切到 Node.js 20.19 或 22（这台机器默认若是 20.15，先执行 `nvm use 22`）：

```bash
npm install
npm run tauri dev
```

打包后的程序不需要再装 Node 或 Rust：

- macOS：`npm run package`，然后双击 `src-tauri/target/release/bundle/macos/PopStall.app`
- Linux：`npm run package`，运行打出来的 `.AppImage`
- Windows 11：在项目根目录执行 `powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1`。安装包在 `src-tauri\target\release\bundle\nsis\`，免安装的程序是 `src-tauri\target\release\popstall.exe`。Windows 11 已带 WebView2

本项目以 Apache License 2.0 授权，见仓库根目录的 `LICENSE`。

发布端选择 `examples/hello-stall`，开张后复制分享码。访问端再开一个客户端，粘贴分享码。同一台电脑也可以开两个进程互相访问。

首次开张会在系统应用数据目录写入 `endpoint.key`（Unix 权限 `0600`）。这是节点身份，不要提交到仓库。

分享码会等中继地址写进票据。没有中继地址时，对方按节点身份重新查找，跨网仍然可以打开。

## 这一阶段包含

- 发布端用 axum 把文件夹托管在 `127.0.0.1` 的随机端口，并拒绝目录穿越和隐藏文件。
- iroh 节点发出 `EndpointTicket` 作为分享码，页面请求走 ALPN `popstall/http/1`。
- 访问端在随机回环端口反代这些请求，并用独立 WebView 打开。该窗口不匹配任何 capability。
- 单摊位最多 20 个并发访客，每个连接最多 32 条并发流，下行约 8 MiB/s。

## 多层局域网与 helper

很多电脑在多层局域网、NAT 或 CGNAT 后面，单靠直连不可达。闪摊优先直连，其次使用 iroh relay；仍不可达时才启用 helper。helper 是有用的转发节点，但**不可信**：helper 桥接的请求和响应在双方之间用 AES-GCM 端到端加密，helper 看不到正文，只能看到必要的路由元数据。界面会明确显示当前路径是直连、relay 还是 helper，不会把 helper 路径伪装成直连。

helper 行为受每会话带宽预算约束。端口映射默认关闭，显式开启后才按顺序尝试 PCP→NAT-PMP→UPnP；失败时回退到 relay。relay-only helper 可以只带 relay 地址参与发现，但不会把 helper 路径伪装成直连，也不需要普通用户手工配置路由器。

## 完整性与恢复

用户发布的本地文件带 BLAKE3 全文摘要。访问端在写入缓存或播放前校验；影音切片校验 `Content-Range` 和缓存内容，摘要不一致会丢弃坏数据并按 `Range` 重试一次。打包进应用的静态页面没有文件摘要，不会伪造一个。

## 广场与降级

广场用 gossip 交换正在营业的摊子和 helper。pkarr HTTP 公告保留，因为它适合 NAT 环境；发布密钥不再写死在源码里，公钥作为固定发现锚点，缺少发布权限时只读降级。发现会尝试多个 relay 并缓存最近成功的 bootstrap 节点；外部网络服务不可用时，已建立的会话仍然保持端到端加密和内容校验。连接尝试有超时和有限退避，重试与路径状态会在网络指标中体现。
