import { Copy, FolderOpen, Power } from "lucide-react";
import { useEffect, useState } from "react";
import { cn, errorText } from "../../lib/utils";
import { useSession } from "../../stores/session";
import type { ScanReport, StallChoice, ThemeId } from "../../types/ipc";
import { StatusDot } from "../common/StatusDot";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { HelperRelayCard } from "./HelperRelayCard";

const themes: { id: ThemeId; label: string; unit: string }[] = [
  { id: "image", label: "图片", unit: "张" },
  { id: "music", label: "音乐", unit: "首" },
  { id: "video", label: "视频", unit: "个" },
  { id: "novel", label: "小说", unit: "篇" },
  { id: "document", label: "文档", unit: "份" },
];

export function HostPanel() {
  const folder = useSession((state) => state.folder);
  const scan = useSession((state) => state.scan);
  const scanning = useSession((state) => state.scanning);
  const choice = useSession((state) => state.choice);
  const port = useSession((state) => state.port);
  const blurb = useSession((state) => state.blurb);
  const setBlurb = useSession((state) => state.setBlurb);
  const password = useSession((state) => state.password);
  const setPassword = useSession((state) => state.setPassword);
  const host = useSession((state) => state.host);
  const busy = useSession((state) => state.hostBusy);
  const chooseFolder = useSession((state) => state.chooseFolder);
  const setChoice = useSession((state) => state.setChoice);
  const setPort = useSession((state) => state.setPort);
  const startHost = useSession((state) => state.startHost);
  const stopHost = useSession((state) => state.stopHost);
  const openPreview = useSession((state) => state.openPreview);
  const refreshHost = useSession((state) => state.refreshHost);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  const [more, setMore] = useState(() => blurb.length > 0 || password.length > 0);
  const [appMode, setAppMode] = useState(() => choice === "app" && !folder);

  useEffect(() => {
    if (folder) {
      setAppMode(false);
    }
  }, [folder]);

  useEffect(() => {
    if (!host || host.relayReady) {
      return undefined;
    }
    const timer = window.setInterval(() => {
      void refreshHost();
    }, 4000);
    return () => window.clearInterval(timer);
  }, [host, refreshHost]);

  async function copyCode() {
    if (!host) {
      return;
    }
    try {
      await navigator.clipboard.writeText(host.shareCode);
      setCopied(true);
      setCopyError(null);
    } catch (error) {
      setCopied(false);
      setCopyError(errorText(error));
    }
  }

  const options = scan ? availableOptions(scan) : [];
  const folderChoice: StallChoice | null = !appMode && choice !== "app" ? choice : null;
  const ready = appMode ? canOpen("app", Boolean(folder), Boolean(scan)) : canOpen(folderChoice, Boolean(folder), Boolean(scan));

  return (
    <div className="grid gap-4 2xl:grid-cols-[minmax(0,1.4fr)_minmax(18rem,0.85fr)] 2xl:items-start">
    <Card className="flex flex-col gap-5">
      {host ? (
        <LiveStall
          modeLabel={host.modeLabel}
          root={host.root}
          endpointId={host.endpointId}
          relayReady={host.relayReady}
          locked={host.locked}
          shareCode={host.shareCode}
          copied={copied}
          copyError={copyError}
          busy={busy}
          onCopy={() => void copyCode()}
          onPreview={() => void openPreview()}
          onStop={() => void stopHost()}
        />
      ) : appMode ? (
        <AppSetup
          port={port}
          busy={busy}
          more={more}
          blurb={blurb}
          password={password}
          onPort={(value) => setPort(value.replace(/[^\d]/g, "").slice(0, 5))}
          onBlurb={setBlurb}
          onPassword={setPassword}
          onToggleMore={() => setMore((open) => !open)}
          onBack={() => {
            setAppMode(false);
            const next = scan ? (availableOptions(scan)[0]?.id ?? null) : null;
            if (next) {
              setChoice(next);
            }
          }}
          onStart={() => void startHost()}
        />
      ) : (
        <>
          <div>
            <h2 className="font-serif text-3xl font-medium tracking-tight">{scan?.folderName ?? "摆一摊"}</h2>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-muted">
              {scan ? scanSummary(scan) : "选一个文件夹。里面是什么，就按什么摆出去。"}
            </p>
          </div>

          <button
            type="button"
            onClick={() => void chooseFolder()}
            disabled={busy || scanning}
            className="flex items-center justify-between gap-4 rounded-[1.3rem] border border-dashed border-line bg-paper/80 px-4 py-4 text-left transition hover:border-[#d7b79a] disabled:cursor-wait"
          >
            <span className="min-w-0">
              <span className="block text-xs tracking-wide text-muted">文件夹</span>
              <span className="mt-1 block truncate font-serif text-xl">
                {scanning ? "正在看里面有什么…" : (scan?.folderName ?? "选择文件夹")}
              </span>
              {folder ? <span className="mt-1 block truncate text-xs text-muted">{folder}</span> : null}
            </span>
            <span className="inline-flex shrink-0 items-center gap-2 text-sm text-ember-dark">
              <FolderOpen className="size-4" />
              {folder ? "换一个" : "选择"}
            </span>
          </button>
          {scan?.truncated ? <p className="text-xs text-ember-dark">文件很多，只认了前 5000 个。</p> : null}

          {options.length > 1 ? (
            <div>
              <p className="text-sm">这一次只摆一种</p>
              <div className="mt-2 flex flex-wrap gap-2" role="radiogroup" aria-label="摆成">
                {options.map((option) => (
                  <button
                    key={option.id}
                    type="button"
                    role="radio"
                    aria-checked={folderChoice === option.id}
                    onClick={() => setChoice(option.id)}
                    className={cn(
                      "rounded-full border px-3 py-1.5 text-sm transition",
                      folderChoice === option.id ? "border-ink bg-ink text-paper" : "border-line bg-card text-ink hover:border-[#d7b79a]",
                    )}
                  >
                    {option.label}
                    <span className={cn("ml-2 font-mono text-xs", folderChoice === option.id ? "text-paper/70" : "text-muted")}>
                      {option.countLabel}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          ) : null}

          {scan && options.length === 0 ? (
            <p className="text-sm leading-6 text-muted">这个文件夹里没有图片、音乐、视频、小说、文档，也没有网站首页。</p>
          ) : null}

          {folderChoice === "site" ? (
            <div className="rounded-2xl border border-line bg-sidebar px-4 py-3 text-[13px] leading-6 text-muted">
              <p className="font-medium text-ink">原样网站 = 只托管静态文件</p>
              <p className="mt-1">
                没有后端进程。若站点依赖 API / 登录 / WebSocket，请改用「转发本机端口」：本机先把前后端跑起来，再填那个端口。
              </p>
            </div>
          ) : null}

          {scan && options.length > 0 ? (
            <Button className="w-full py-2.5" onClick={() => void startHost()} disabled={busy || scanning || !ready}>
              {busy ? "正在开张" : openLabel(folderChoice, scan)}
            </Button>
          ) : null}

          <OptionalFields
            open={more}
            blurb={blurb}
            password={password}
            onToggle={() => setMore((value) => !value)}
            onBlurb={setBlurb}
            onPassword={setPassword}
          />

          <button
            type="button"
            onClick={() => {
              setAppMode(true);
              setChoice("app");
            }}
            className="rounded-2xl border border-dashed border-line-strong bg-card/80 px-4 py-3 text-left transition hover:border-ember/40 hover:bg-clay/40"
          >
            <span className="block text-[13px] font-medium text-ink">有后端 API？转发本机端口</span>
            <span className="mt-1 block text-[12px] leading-5 text-muted">
              Next / Vite / Docker / 本地服务先在本机跑着，闪摊只做加密转发。适合带接口的完整网站。
            </span>
          </button>
        </>
      )}
    </Card>
    <HelperRelayCard />
    </div>
  );
}

function AppSetup({
  port,
  busy,
  more,
  blurb,
  password,
  onPort,
  onBlurb,
  onPassword,
  onToggleMore,
  onBack,
  onStart,
}: {
  port: string;
  busy: boolean;
  more: boolean;
  blurb: string;
  password: string;
  onPort: (value: string) => void;
  onBlurb: (value: string) => void;
  onPassword: (value: string) => void;
  onToggleMore: () => void;
  onBack: () => void;
  onStart: () => void;
}) {
  return (
    <>
      <div>
        <h2 className="font-serif text-3xl font-medium tracking-tight">转发本机端口</h2>
        <p className="mt-2 max-w-2xl text-sm leading-6 text-muted">
          本机先跑完整站点（前端 + API），再填监听端口。页面、同源接口、WebSocket 都会经闪摊加密转给访客。
        </p>
      </div>
      <div className="rounded-2xl border border-line bg-sidebar px-4 py-3 text-[12px] leading-5 text-muted">
        <p>· 前端请求请用相对路径（如 <code className="font-mono text-[11px] text-ink">/api/...</code>），不要写死其它 localhost 端口。</p>
        <p className="mt-1">· 只转发你信任的开发服或自建站点；不要转发数据库、管理后台或未鉴权服务。</p>
        <p className="mt-1">· 访客侧是系统 WebView（macOS WKWebView / Windows WebView2），不是完整 Chrome 扩展环境。</p>
      </div>
      <label className="flex items-center justify-between gap-3 rounded-[1.3rem] border border-line bg-paper px-4 py-4">
        <span>
          <span className="block text-xs text-muted">本机端口</span>
          <span className="mt-1 block text-sm text-muted">例如 Vite 5173、Next 3000、你自己的后端端口。</span>
        </span>
        <input
          value={port}
          inputMode="numeric"
          aria-label="端口"
          onChange={(event) => onPort(event.target.value)}
          className="w-24 rounded-full border border-line bg-card px-3 py-2 text-center text-ink outline-none focus:border-ember"
        />
      </label>
      <Button className="w-full py-2.5" onClick={onStart} disabled={busy}>
        {busy ? "正在开张" : "开始转发"}
      </Button>
      <OptionalFields open={more} blurb={blurb} password={password} onToggle={onToggleMore} onBlurb={onBlurb} onPassword={onPassword} />
      <button type="button" onClick={onBack} className="self-start text-sm text-muted hover:text-ink">
        改回选择文件夹
      </button>
    </>
  );
}

function OptionalFields({
  open,
  blurb,
  password,
  onToggle,
  onBlurb,
  onPassword,
}: {
  open: boolean;
  blurb: string;
  password: string;
  onToggle: () => void;
  onBlurb: (value: string) => void;
  onPassword: (value: string) => void;
}) {
  return (
    <div>
      <button type="button" onClick={onToggle} className="text-sm text-ember-dark">
        {open ? "收起一句话和密码" : "给广场写一句，或设个密码"}
      </button>
      {open ? (
        <div className="mt-3 flex flex-col gap-3">
          <label className="block">
            <span className="flex items-baseline justify-between gap-3 text-sm">
              <span>给广场的一句话</span>
              <span className="font-mono text-xs text-muted">{Array.from(blurb).length}/100</span>
            </span>
            <textarea
              value={blurb}
              maxLength={100}
              rows={2}
              placeholder="别人按这里的词找你，比如：爵士、夜里、民谣"
              onChange={(event) => onBlurb(event.target.value)}
              className="mt-2 w-full resize-none rounded-[1.1rem] border border-line bg-paper px-4 py-3 text-sm leading-6 outline-none focus:border-ember"
            />
          </label>
          <label className="block">
            <span className="text-sm">密码</span>
            <input
              type="password"
              value={password}
              autoComplete="new-password"
              maxLength={48}
              placeholder="留空就是公开摊"
              onChange={(event) => onPassword(event.target.value)}
              className="mt-2 w-full rounded-full border border-line bg-paper px-4 py-2 text-sm outline-none focus:border-ember"
            />
            <span className="mt-2 block text-xs leading-5 text-muted">广场上仍看得见摊名和那句话。密码别写进那句话里。</span>
          </label>
        </div>
      ) : null}
    </div>
  );
}

function canOpen(choice: StallChoice | null, hasFolder: boolean, hasScan: boolean) {
  if (!choice) {
    return false;
  }
  if (choice === "app") {
    return true;
  }
  return hasFolder && hasScan;
}

function availableOptions(scan: ScanReport): { id: StallChoice; label: string; countLabel: string }[] {
  const options: { id: StallChoice; label: string; countLabel: string }[] = [];
  for (const theme of themes) {
    const count = scan.counts[theme.id];
    if (count > 0) {
      options.push({ id: theme.id, label: theme.label, countLabel: `${count} ${theme.unit}` });
    }
  }
  if (scan.hasIndex) {
    options.push({ id: "site", label: "原样网站", countLabel: "有首页" });
  }
  return options;
}

function scanSummary(scan: ScanReport) {
  const parts: string[] = [];
  for (const theme of themes) {
    const count = scan.counts[theme.id];
    if (count > 0) {
      parts.push(`${count} ${theme.unit}${theme.label}`);
    }
  }
  if (scan.hasIndex) {
    parts.push("有网站首页");
  }
  if (scan.counts.other > 0) {
    parts.push(`${scan.counts.other} 个不摆出的文件`);
  }
  if (parts.length === 0) {
    return "没有认出可以摆出的内容。";
  }
  if (parts.length === 1) {
    return `${parts[0]}。`;
  }
  return `${parts.join("，")}。这一次只摆一种，认出来的第一种会先被选中。`;
}

function openLabel(choice: StallChoice | null, scan: ScanReport | null) {
  if (!choice || choice === "app") {
    return "先选择文件夹";
  }
  if (choice === "site") {
    return "按网站摆出";
  }
  const theme = themes.find((item) => item.id === choice);
  const count = scan?.counts[choice] ?? 0;
  if (!theme || count <= 0) {
    return "开张";
  }
  return `摆出这 ${count} ${theme.unit}${theme.label}`;
}

function LiveStall({
  modeLabel,
  root,
  endpointId,
  relayReady,
  locked,
  shareCode,
  copied,
  copyError,
  busy,
  onCopy,
  onPreview,
  onStop,
}: {
  modeLabel: string;
  root: string;
  endpointId: string;
  relayReady: boolean;
  locked: boolean;
  shareCode: string;
  copied: boolean;
  copyError: string | null;
  busy: boolean;
  onCopy: () => void;
  onPreview: () => void;
  onStop: () => void;
}) {
  const name = root.split(/[/\\]/).filter(Boolean).pop() || root;
  return (
    <div className="flex flex-col gap-4">
      <div className="rounded-[1.3rem] bg-moss px-4 py-4">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <p className="truncate font-serif text-2xl">{name}</p>
            <p className="mt-1 text-sm text-pine">{modeLabel}</p>
          </div>
          <StatusDot tone="live" label={locked ? "私密营业" : "营业中"} />
        </div>
      </div>

      <div className="flex flex-wrap gap-3">
        <Button className="flex-1" onClick={onPreview}>
          先看一眼
        </Button>
        <Button variant="quiet" className="flex-1" onClick={onCopy}>
          <Copy className="size-4" />
          {copied ? "已复制" : "复制分享码"}
        </Button>
      </div>
      <p className="text-xs leading-5 text-muted">
        {relayReady ? "广场上的人现在就能进来。分享码留给广场上看不见你的朋友。" : "中继还在接上。同一网络可以先用分享码，接上之后全球都能打开。"}
      </p>
      {copyError ? <p className="text-xs text-ember-dark">{copyError}，可以展开分享码手动复制。</p> : null}

      <details className="rounded-[1.1rem] border border-line bg-paper px-4 py-3">
        <summary className="cursor-pointer text-sm text-muted">查看分享码</summary>
        <textarea readOnly className="mt-3 min-h-24 w-full resize-y bg-transparent font-mono text-[11px] leading-5 text-ink outline-none" value={shareCode} />
        <p className="mt-1 break-all font-mono text-[11px] text-muted">{endpointId}</p>
      </details>

      <Button variant="danger" className="self-start" onClick={onStop} disabled={busy}>
        <Power className="size-4" />
        {busy ? "正在打烊" : "打烊"}
      </Button>
    </div>
  );
}
