import { useEffect, useState } from "react";
import { useSession } from "../../stores/session";
import { cn, errorText } from "../../lib/utils";
import { Button } from "../ui/button";

export function MyStallCard({ active, onOpen }: { active: boolean; onOpen: () => void }) {
  const host = useSession((state) => state.host);
  const blurb = useSession((state) => state.blurb);
  const openedAt = useSession((state) => state.openedAt);
  const busy = useSession((state) => state.hostBusy);
  const openPreview = useSession((state) => state.openPreview);
  const stopHost = useSession((state) => state.stopHost);
  const [now, setNow] = useState(() => Date.now());
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);

  useEffect(() => {
    if (!openedAt) {
      return undefined;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 30000);
    return () => window.clearInterval(timer);
  }, [openedAt]);

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
    <div className={cn("rounded-2xl border px-3 py-3", host ? "border-pine/25 bg-moss" : "border-dashed border-line bg-paper", active && "ring-1 ring-ember")}>
      <button type="button" onClick={onOpen} className="w-full text-left">
        {host ? (
          <>
            <span className="flex items-center gap-2 text-xs text-pine">
              <span className="relative flex size-2">
                <span className="absolute inline-flex size-full animate-ping rounded-full bg-pine opacity-60 motion-reduce:animate-none" />
                <span className="relative size-2 rounded-full bg-pine" />
              </span>
              {host.locked ? "私密营业" : "营业中"}
            </span>
            <span className="mt-1 block truncate font-serif text-xl">{stallName(host.root)}</span>
            <span className="mt-1 block truncate text-xs text-muted">
              {host.modeLabel}
              {openedAt ? ` · ${formatStay(openedAt, now)}` : ""}
            </span>
            {blurb ? <span className="mt-2 block line-clamp-2 text-xs leading-5 text-ink">{blurb}</span> : null}
            {host.relayReady ? null : <span className="mt-1 block text-xs text-ember-dark">中继还在接上</span>}
          </>
        ) : (
          <>
            <span className="block font-serif text-xl">今晚还没摆摊</span>
            <span className="mt-1 block text-xs leading-5 text-muted">选一个文件夹，广场上的人就能进来。</span>
          </>
        )}
      </button>
      {host ? (
        <div className="mt-3 flex flex-col gap-2">
          <div className="flex gap-2">
            <Button variant="quiet" className="flex-1 px-3 py-1.5 text-xs" onClick={() => void openPreview()}>
              预览
            </Button>
            <Button variant="danger" className="flex-1 px-3 py-1.5 text-xs" disabled={busy} onClick={() => void stopHost()}>
              {busy ? "打烊中" : "打烊"}
            </Button>
          </div>
          <button type="button" onClick={() => void copyCode()} className="text-left text-xs text-pine">
            {copied ? "分享码已复制" : "复制分享码"}
          </button>
          {copyError ? <p className="text-xs text-ember-dark">{copyError}</p> : null}
        </div>
      ) : (
        <Button className="mt-3 w-full" onClick={onOpen}>
          摆一摊
        </Button>
      )}
    </div>
  );
}

function stallName(root: string) {
  const parts = root.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] || root;
}

function formatStay(openedAt: number, now: number) {
  const minutes = Math.max(1, Math.round((now - openedAt) / 60000));
  if (minutes < 60) {
    return `已摆 ${minutes} 分钟`;
  }
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest > 0 ? `已摆 ${hours} 小时 ${rest} 分` : `已摆 ${hours} 小时`;
}
