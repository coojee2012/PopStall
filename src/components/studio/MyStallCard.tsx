import { Copy, Eye, Store } from "lucide-react";
import { useEffect, useState } from "react";
import { useSession } from "../../stores/session";
import { cn, errorText } from "../../lib/utils";
import { Button } from "../ui/button";
import { toast } from "../ui/toast";

export function MyStallCard({ active, onOpen }: { active: boolean; onOpen: () => void }) {
  const host = useSession((state) => state.host);
  const blurb = useSession((state) => state.blurb);
  const openedAt = useSession((state) => state.openedAt);
  const busy = useSession((state) => state.hostBusy);
  const openPreview = useSession((state) => state.openPreview);
  const stopHost = useSession((state) => state.stopHost);
  const [now, setNow] = useState(() => Date.now());

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
      toast("分享码已复制");
    } catch (error) {
      toast(errorText(error), "warn");
    }
  }

  return (
    <div
      className={cn(
        "rounded-2xl border border-line bg-card p-3 shadow-[0_1px_2px_rgba(28,25,23,0.04)]",
        active && "ring-1 ring-ember/30",
      )}
    >
      <button type="button" onClick={onOpen} className="w-full text-left">
        {host ? (
          <>
            <span className="flex items-center gap-2 text-[11px] font-medium text-pine">
              <span className="live-dot size-1.5 rounded-full bg-pine" />
              {host.locked ? "私密营业" : "营业中"}
            </span>
            <span className="mt-1.5 block truncate text-[15px] font-medium tracking-tight text-ink">
              {stallName(host.root)}
            </span>
            <span className="mt-1 block truncate text-[12px] text-muted">
              {host.modeLabel}
              {openedAt ? ` · ${formatStay(openedAt, now)}` : ""}
            </span>
            {blurb ? (
              <span className="mt-2 block line-clamp-2 text-[12px] leading-5 text-ink/80">{blurb}</span>
            ) : null}
            {host.relayReady ? null : (
              <span className="mt-1 block text-[11px] text-ember-dark">中继还在接上</span>
            )}
          </>
        ) : (
          <>
            <span className="flex items-center gap-2 text-[11px] font-medium text-muted">
              <Store className="size-3.5" />
              我的摊位
            </span>
            <span className="mt-1.5 block text-[15px] font-medium tracking-tight text-ink">今晚还没摆摊</span>
            <span className="mt-1 block text-[12px] leading-5 text-muted">
              选文件夹或映射端口，广场上的人就能进来。
            </span>
          </>
        )}
      </button>
      {host ? (
        <div className="mt-3 grid grid-cols-3 gap-1.5">
          <Button
            variant="quiet"
            size="sm"
            className="gap-1 px-2"
            onClick={() => void openPreview()}
          >
            <Eye className="size-3.5" />
            预览
          </Button>
          <Button
            variant="quiet"
            size="sm"
            className="gap-1 px-2"
            onClick={() => void copyCode()}
          >
            <Copy className="size-3.5" />
            分享
          </Button>
          <Button
            variant="danger"
            size="sm"
            className="px-2"
            disabled={busy}
            onClick={() => void stopHost()}
          >
            {busy ? "…" : "打烊"}
          </Button>
        </div>
      ) : (
        <button
          type="button"
          onClick={onOpen}
          className="mt-3 w-full rounded-xl border border-line px-3 py-2 text-left text-[12px] text-muted transition hover:bg-sidebar hover:text-ink"
        >
          管理摊位设置 →
        </button>
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
