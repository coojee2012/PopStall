# IPC 契约（Phase 1）

命令都由主窗口 `main` 调用。访客窗口没有任何 capability。参数在 JavaScript 里用 camelCase。

| 命令 | 参数 | 返回 |
| --- | --- | --- |
| `start_host` | `root: string` | `HostInfo` |
| `stop_host` | 无 | `void` |
| `host_status` | 无 | `HostInfo \| null` |
| `open_guest` | `shareCode: string` | `GuestInfo` |
| `close_guest` | `windowLabel: string` | `void` |

```ts
type HostInfo = {
  shareCode: string;
  endpointId: string;
  localUrl: string;
  root: string;
  relayReady: boolean;
};

type GuestInfo = {
  windowLabel: string;
  proxyUrl: string;
  endpointId: string;
};
```

文件夹选择走 Tauri dialog 插件，不经过上面的命令。计划书里的广场、屏蔽和摊位配置命令留到后续阶段。
