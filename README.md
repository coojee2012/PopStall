# 闪摊 PopStall

Phase 1 桌面原型：把一个本地静态文件夹开成摊位，生成 iroh 分享码；另一台客户端粘贴分享码后，在没有 Tauri 权限的窗口里打开对方页面。

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
- 单摊位最多 20 个并发访客，每个连接最多 32 条并发流，下行约 4 MiB/s。

广场用 gossip 交换正在营业的摊位。内容仍然点对点传输，不经过闪摊自己的服务器。
