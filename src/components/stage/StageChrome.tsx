import { ArrowLeft, ExternalLink } from "lucide-react";
import type { ReactNode, Ref } from "react";
import { kindMeta, stallKind } from "../../lib/plaza";
import { cn } from "../../lib/utils";
import type { StageInfo } from "../../types/ipc";
import { RouteBanner } from "../studio/RouteBanner";
import { StatusCapsule } from "../studio/StatusCapsule";

/**
 * Fixed top chrome above the native child webview.
 * Keep height stable — StageView measures this for place_stage.
 */
export function StageChrome({
  stage,
  busy,
  onBack,
  chromeRef,
  extra,
}: {
  stage: StageInfo;
  busy: boolean;
  onBack: () => void;
  chromeRef?: Ref<HTMLDivElement>;
  extra?: ReactNode;
}) {
  const kind = stallKind(stage.label || "");
  const meta = kindMeta(kind);
  const Icon = meta.Icon;
  const isSite = kind === "site" || kind === "app";

  return (
    <div ref={chromeRef} className="relative z-50 shrink-0 border-b border-line bg-card">
      <header className="flex h-12 items-center gap-3 px-3 sm:px-4">
        <button
          type="button"
          onClick={onBack}
          disabled={busy}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-line bg-sidebar px-2.5 py-1.5 text-[12px] font-medium text-ink transition hover:bg-paper disabled:opacity-50"
        >
          <ArrowLeft className="size-3.5" strokeWidth={1.75} />
          返回广场
        </button>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
            <p className="truncate text-[14px] font-semibold tracking-tight">{stage.title}</p>
            <span
              className={cn(
                "inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-medium",
                meta.chip,
              )}
            >
              <Icon className="size-3" strokeWidth={1.75} />
              {meta.label}
            </span>
          </div>
          <p className="truncate text-[11px] text-muted">
            {stage.kind === "guest" ? "串门中 · 本地直连" : "本机预览"}
            {stage.label ? ` · ${stage.label}` : ""}
          </p>
        </div>
        {isSite ? (
          <span className="hidden items-center gap-1 text-[11px] text-soft sm:inline-flex">
            <ExternalLink className="size-3" strokeWidth={1.75} />
            沙箱内浏览
          </span>
        ) : null}
        {extra}
        <StatusCapsule />
      </header>
      <RouteBanner />
    </div>
  );
}
