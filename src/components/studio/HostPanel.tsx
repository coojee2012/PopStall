import { Copy, FolderOpen, Power } from "lucide-react";
import { useEffect, useState } from "react";
import { cn, errorText } from "../../lib/utils";
import { useSession } from "../../stores/session";
import type { StallChoice, ThemeId } from "../../types/ipc";
import { StatusDot } from "../common/StatusDot";
import { Button } from "../ui/button";
import { Card } from "../ui/card";

const themes: { id: ThemeId; label: string; hint: string }[] = [
  { id: "image", label: "图片", hint: "网格，点开大图" },
  { id: "music", label: "音乐", hint: "曲目，手动播放" },
  { id: "video", label: "视频", hint: "宽幕，可拖进度" },
  { id: "novel", label: "小说", hint: "章节与纸页" },
  { id: "document", label: "文档", hint: "PDF 翻阅" },
];

export function HostPanel() {
  const folder = useSession((state) => state.folder);
  const scan = useSession((state) => state.scan);
  const scanning = useSession((state) => state.scanning);
  const choice = useSession((state) => state.choice);
  const port = useSession((state) => state.port);
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

  return (
    <Card className="flex flex-col gap-6">
      <div>
        <p className="text-[0.68rem] tracking-[0.28em] text-ember">开张</p>
        <h2 className="mt-1 font-serif text-3xl font-medium tracking-tight">今晚摆什么</h2>
        <p className="mt-2 max-w-xl text-sm leading-6 text-muted">
          文件夹里什么都可能有。先选定一种，访客就只看见这一类，并且按它该有的样子打开。
        </p>
      </div>

      {host ? (
        <LiveStall
          modeLabel={host.modeLabel}
          root={host.root}
          endpointId={host.endpointId}
          relayReady={host.relayReady}
          shareCode={host.shareCode}
          copied={copied}
          copyError={copyError}
          busy={busy}
          onCopy={() => void copyCode()}
          onPreview={() => void openPreview()}
          onStop={() => void stopHost()}
        />
      ) : (
        <>
          <div className="rounded-[1.3rem] border border-dashed border-line bg-paper/80 px-4 py-4">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="min-w-0">
                <p className="text-xs tracking-wide text-muted">文件夹</p>
                <p className="mt-1 truncate font-serif text-xl">{scan?.folderName ?? "还没有选择"}</p>
              </div>
              <Button variant="quiet" onClick={() => void chooseFolder()} disabled={busy || scanning}>
                <FolderOpen className="size-4" />
                {folder ? "换一个" : "选择文件夹"}
              </Button>
            </div>
            <p className="mt-2 break-all text-xs leading-5 text-muted">
              {scanning ? "正在辨认里面的文件…" : (folder ?? "图片、音乐、小说、网站，或者只是一堆混在一起的文件。")}
            </p>
            {scan?.truncated ? <p className="mt-2 text-xs text-ember-dark">文件很多，只统计了前 5000 个。</p> : null}
          </div>

          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
            {themes.map((theme) => (
              <ChoiceCard
                key={theme.id}
                label={theme.label}
                hint={theme.hint}
                count={scan ? scan.counts[theme.id] : null}
                selected={choice === theme.id}
                disabled={!scan || scan.counts[theme.id] === 0}
                onSelect={() => setChoice(theme.id)}
              />
            ))}
            <ChoiceCard
              label="原样网站"
              hint="有 index.html"
                count={scan ? (scan.hasIndex ? "有" : "无") : null}
              selected={choice === "site"}
              disabled={!scan?.hasIndex}
              onSelect={() => setChoice("site")}
            />
          </div>

          <div
            className={cn(
              "rounded-[1.3rem] border px-4 py-4 transition",
              choice === "app" ? "border-ember bg-clay/70" : "border-line bg-paper/70",
            )}
          >
            <div className="flex flex-wrap items-center justify-between gap-3">
              <button type="button" className="text-left" onClick={() => setChoice("app")}>
                <span className="font-serif text-xl">本地应用</span>
                <span className="mt-1 block text-xs text-muted">把已经跑在本机的网页转发出去</span>
              </button>
              <label className="flex items-center gap-2 text-sm text-muted">
                端口
                <input
                  value={port}
                  inputMode="numeric"
                  onFocus={() => setChoice("app")}
                  onChange={(event) => {
                    setChoice("app");
                    setPort(event.target.value.replace(/[^\d]/g, "").slice(0, 5));
                  }}
                  className="w-24 rounded-full border border-line bg-card px-3 py-2 text-ink outline-none focus:border-ember"
                />
              </label>
            </div>
              <p className={cn("mt-3 text-xs leading-5", choice === "app" ? "text-ember-dark" : "text-muted")}>
                对方的请求到你电脑上时，这个程序会以为是你自己在访问。不要转发数据库、后台，或默认信任本机的服务。本机预览会连上实时通道；朋友从分享码进来时，页面和接口可以打开，依赖
                WebSocket 的热更新还不行。
              </p>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-3">
            <StatusDot tone={busy || scanning ? "busy" : "idle"} label={busy ? "正在准备节点" : scanning ? "正在看文件夹" : "还没有开张"} />
            <Button onClick={() => void startHost()} disabled={busy || scanning || !canOpen(choice, Boolean(folder), Boolean(scan))}>
              {busy ? "正在开张" : "开张"}
            </Button>
          </div>
        </>
      )}
    </Card>
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

function ChoiceCard({
  label,
  hint,
  count,
  selected,
  disabled,
  onSelect,
}: {
  label: string;
  hint: string;
  count: number | string | null;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onSelect}
      className={cn(
        "rounded-2xl border px-3 py-3 text-left transition",
        selected ? "border-ember bg-clay" : "border-line bg-paper hover:border-[#d7b79a]",
        disabled && "cursor-not-allowed opacity-40 hover:border-line",
      )}
    >
      <span className="flex items-baseline justify-between gap-2">
        <span className="font-serif text-xl">{label}</span>
        <span className="font-mono text-xs text-muted">{count === null ? "—" : count}</span>
      </span>
      <span className="mt-1 block text-xs text-muted">{hint}</span>
    </button>
  );
}

function LiveStall({
  modeLabel,
  root,
  endpointId,
  relayReady,
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
  shareCode: string;
  copied: boolean;
  copyError: string | null;
  busy: boolean;
  onCopy: () => void;
  onPreview: () => void;
  onStop: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      <div className="rounded-[1.3rem] bg-moss px-4 py-4">
        <div className="flex items-start justify-between gap-3">
          <div>
            <p className="text-xs tracking-wide text-pine">{modeLabel}</p>
            <p className="mt-1 break-all font-serif text-2xl">{root.split("/").filter(Boolean).pop() || root}</p>
          </div>
          <StatusDot tone="live" label="营业中" />
        </div>
        <p className="mt-2 break-all text-xs leading-5 text-muted">{root}</p>
      </div>

      <div className="rounded-[1.3rem] border border-line bg-paper px-4 py-4">
        <div className="flex items-center justify-between gap-3">
          <p className="text-sm">把分享码发给朋友</p>
          <Button variant="quiet" onClick={onCopy}>
            <Copy className="size-4" />
            {copied ? "已复制" : "复制"}
          </Button>
        </div>
        <textarea
          readOnly
          className="mt-3 min-h-24 w-full resize-y bg-transparent font-mono text-[11px] leading-5 text-ink outline-none"
          value={shareCode}
        />
        <p className="text-xs leading-5 text-muted">
          {relayReady
            ? "分享码里已经有中继地址。世界上任何地方，只要对方开着闪摊，贴上就能打开。"
            : "中继地址还没写进分享码。接上之后这里会自动换成全球可打开的码；在那之前，同一网络可以先用。"}
        </p>
        <p className="mt-1 break-all font-mono text-[11px] text-muted">{endpointId}</p>
        {copyError ? <p className="mt-2 text-xs text-ember-dark">{copyError}，可以手动选中分享码复制。</p> : null}
      </div>

      <div className="flex flex-wrap gap-3">
        <Button variant="quiet" onClick={onPreview}>
          在本机预览
        </Button>
        <Button variant="danger" onClick={onStop} disabled={busy}>
          <Power className="size-4" />
          {busy ? "正在打烊" : "打烊"}
        </Button>
      </div>
    </div>
  );
}
