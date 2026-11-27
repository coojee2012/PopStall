import { ChevronDown, Copy, Radio } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { traffic } from "../../lib/api";
import { useRouteStore } from "../../stores/route";
import { useSession } from "../../stores/session";
import { cn, errorText, routeStatusNote } from "../../lib/utils";
import { toast } from "../ui/toast";

export function StatusCapsule() {
  const host = useSession((state) => state.host);
  const stage = useSession((state) => state.stage);
  const pathLabel = useRouteStore((state) => state.pathLabel);
  const pathTrust = useRouteStore((state) => state.pathTrust);
  const relayOnly = useRouteStore((state) => state.relayOnly);
  const viaHelper = useRouteStore((state) => state.viaHelper);
  const digestVerified = useRouteStore((state) => state.digestVerified);
  const retryCount = useRouteStore((state) => state.retryCount);
  const degraded = useRouteStore((state) => state.degraded);
  const [open, setOpen] = useState(false);
  const [rates, setRates] = useState({ up: 0, down: 0 });
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const history: { sent: number; received: number; at: number }[] = [];
    let alive = true;
    const tick = async () => {
      try {
        const sample = await traffic();
        const at = performance.now();
        history.push({ sent: sample.sent, received: sample.received, at });
        while (history.length > 1 && history[0].at < at - 4000) {
          history.shift();
        }
        const first = history[0];
        const seconds = (at - first.at) / 1000;
        if (alive && seconds >= 0.8) {
          setRates({
            up: Math.max(0, (sample.sent - first.sent) / seconds),
            down: Math.max(0, (sample.received - first.received) / seconds),
          });
        }
      } catch {
        if (alive) {
          setRates({ up: 0, down: 0 });
        }
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 1000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, []);

  useEffect(() => {
    if (!open) {
      return;
    }
    const onDoc = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  const live = Boolean(host);
  const active = Boolean(host || stage);
  const status = routeStatusNote({
    pathTrust,
    pathLabel,
    viaHelper,
    relayOnly,
    digestVerified,
    retryCount,
    degraded,
  });
  const showCapsuleWarnings =
    active && status.warnings.length > 0 && status.kind !== "helper" && status.kind !== "relay";

  async function copyShare() {
    if (!host) {
      return;
    }
    try {
      await navigator.clipboard.writeText(host.shareCode);
      toast("分享码已复制");
      setOpen(false);
    } catch (error) {
      toast(errorText(error), "warn");
    }
  }

  return (
    <div ref={root} className="relative shrink-0">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        className={cn(
          "inline-flex items-center gap-2 rounded-full border border-line bg-card px-3 py-1.5 text-left shadow-[0_1px_2px_rgba(28,25,23,0.04)] transition hover:border-line-strong",
          open && "border-line-strong",
        )}
      >
        <span
          className={cn(
            "size-1.5 rounded-full",
            live ? "live-dot bg-pine" : "bg-soft",
          )}
        />
        <span className="text-[12px] font-medium text-ink">{live ? "营业中" : "未开摊"}</span>
        {showCapsuleWarnings ? (
          <span
            className="rounded-full bg-clay px-1.5 py-0.5 text-[10px] font-medium text-ember-dark"
            title={status.warnings.join(" · ")}
          >
            需检查
          </span>
        ) : null}
        <span className="hidden h-3 w-px bg-line sm:block" />
        <span className="hidden font-mono text-[11px] text-muted tabular-nums sm:inline">
          ↑ {formatRate(rates.up)}
        </span>
        <span className="hidden font-mono text-[11px] text-muted tabular-nums sm:inline">
          ↓ {formatRate(rates.down)}
        </span>
        <ChevronDown className={cn("size-3.5 text-soft transition", open && "rotate-180")} />
      </button>
      {open ? (
        <div className="absolute top-[calc(100%+8px)] right-0 z-40 w-72 rounded-2xl border border-line bg-card p-3 shadow-[0_16px_40px_rgba(28,25,23,0.12)]">
          <div className="flex items-start gap-2 px-1">
            <Radio className="mt-0.5 size-4 text-muted" />
            <div className="min-w-0">
              <p className="text-[13px] font-medium text-ink">{live ? host?.modeLabel : "还没摆摊"}</p>
              <p className="mt-0.5 text-[12px] text-muted">路径 · {status.label}</p>
              {showCapsuleWarnings ? (
                <div className="mt-1 flex flex-wrap gap-1">
                  {status.warnings.map((warning) => (
                    <span
                      key={warning}
                      className="rounded-full bg-clay px-2 py-0.5 text-[11px] text-ember-dark"
                    >
                      {warning}
                    </span>
                  ))}
                </div>
              ) : null}
            </div>
          </div>
          <div className="mt-3 grid grid-cols-2 gap-2 rounded-xl bg-sidebar px-3 py-2.5">
            <div>
              <p className="text-[11px] text-soft">上传</p>
              <p className="mt-0.5 font-mono text-[13px] tabular-nums text-ink">{formatRate(rates.up)}</p>
            </div>
            <div>
              <p className="text-[11px] text-soft">下载</p>
              <p className="mt-0.5 font-mono text-[13px] tabular-nums text-ink">{formatRate(rates.down)}</p>
            </div>
          </div>
          {host ? (
            <button
              type="button"
              onClick={() => void copyShare()}
              className="mt-2 inline-flex w-full items-center justify-center gap-2 rounded-xl border border-line px-3 py-2 text-[12px] font-medium text-ink hover:bg-sidebar"
            >
              <Copy className="size-3.5" />
              复制分享码
            </button>
          ) : (
            <p className="mt-2 px-1 text-[12px] leading-5 text-muted">开摊后这里会出现分享码与实时速率。</p>
          )}
        </div>
      ) : null}
    </div>
  );
}

function formatRate(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec < 1) {
    return "0 B/s";
  }
  if (bytesPerSec < 1024) {
    return `${Math.round(bytesPerSec)} B/s`;
  }
  if (bytesPerSec < 1024 * 1024) {
    return `${(bytesPerSec / 1024).toFixed(1)} KB/s`;
  }
  return `${(bytesPerSec / (1024 * 1024)).toFixed(1)} MB/s`;
}
