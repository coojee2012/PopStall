import { Star } from "lucide-react";
import { memo } from "react";
import { togglePin } from "../../lib/memory";
import { kindMeta, kindName, stallKind, stallSubtitle } from "../../lib/plaza";
import { cn } from "../../lib/utils";
import type { PlazaStall } from "../../types/ipc";
import { Button } from "../ui/button";

export type StallCardModel = PlazaStall & {
  live: boolean;
  freshAt: number;
  pinned: boolean;
};

export const StallCard = memo(function StallCard({
  stall,
  busy,
  justSeen,
  onOpen,
}: {
  stall: StallCardModel;
  busy: boolean;
  justSeen: boolean;
  onOpen: () => void;
}) {
  const kind = stallKind(stall.label);
  const meta = kindMeta(kind);
  const Icon = meta.Icon;
  const fresh = stall.live && stall.freshAt > Date.now();
  const subtitle = stallSubtitle(stall);
  const chips = [
    kindName(kind),
    stall.locked ? "要密码" : null,
    fresh ? "刚出现" : null,
    stall.live ? null : "今晚没开",
    justSeen ? "刚看过" : null,
  ].filter(Boolean);

  return (
    <article
      role="button"
      tabIndex={stall.live ? 0 : -1}
      onClick={() => {
        if (stall.live && !busy) {
          onOpen();
        }
      }}
      onKeyDown={(event) => {
        if (!stall.live || busy) {
          return;
        }
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onOpen();
        }
      }}
      className={cn(
        "group flex flex-col rounded-2xl border border-line bg-card p-4 text-left shadow-[0_1px_2px_rgba(28,25,23,0.04)] transition",
        stall.live
          ? "cursor-pointer hover:border-line-strong hover:shadow-[0_8px_24px_rgba(28,25,23,0.06)]"
          : "cursor-default opacity-60",
        justSeen && stall.live && "ring-1 ring-ember/25",
      )}
    >
      <div className="flex items-start gap-3">
        <span
          className={cn(
            "grid size-10 shrink-0 place-items-center rounded-xl",
            meta.wash,
          )}
          aria-hidden
        >
          <Icon className="size-4" strokeWidth={1.75} />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <h3 className="min-w-0 truncate text-[15px] font-semibold tracking-tight text-ink">
                  {stall.title}
                </h3>
                {stall.live ? (
                  <span className="inline-flex items-center gap-1 text-[10px] font-medium text-pine">
                    <span className="live-dot size-1.5 rounded-full bg-pine" />
                    在线
                  </span>
                ) : null}
              </div>
              <span
                className={cn(
                  "mt-1.5 inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium",
                  meta.chip,
                )}
              >
                {kindName(kind)}
              </span>
            </div>
            <button
              type="button"
              aria-label={stall.pinned ? "从常去拿下" : "标成常去"}
              onClick={(event) => {
                event.stopPropagation();
                togglePin(stall);
              }}
              className="grid size-7 shrink-0 place-items-center rounded-full text-soft opacity-70 transition hover:bg-sidebar hover:text-ink group-hover:opacity-100"
            >
              <Star
                className={cn("size-3.5", stall.pinned ? "fill-ember text-ember" : "")}
                strokeWidth={1.75}
              />
            </button>
          </div>
          <p className="mt-2 line-clamp-2 min-h-10 text-[12px] leading-5 text-muted">
            {subtitle}
          </p>
        </div>
      </div>
      <div className="mt-3 flex items-center justify-between gap-2 border-t border-line/70 pt-3">
        <p className="truncate text-[11px] text-soft">{chips.join(" · ")}</p>
        {stall.live ? (
          <Button
            size="sm"
            variant="quiet"
            disabled={busy}
            className="shrink-0"
            onClick={(event) => {
              event.stopPropagation();
              onOpen();
            }}
          >
            进入
          </Button>
        ) : null}
      </div>
    </article>
  );
});

export const StallRow = memo(function StallRow({
  stall,
  busy,
  justSeen,
  onOpen,
}: {
  stall: StallCardModel;
  busy: boolean;
  justSeen: boolean;
  onOpen: () => void;
}) {
  const kind = stallKind(stall.label);
  const meta = kindMeta(kind);
  const Icon = meta.Icon;
  const fresh = stall.live && stall.freshAt > Date.now();
  const chips = [
    stall.label || kindName(kind),
    stall.locked ? "要密码" : null,
    fresh ? "刚出现" : null,
    justSeen ? "刚刚看过" : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      className={cn(
        "flex h-full items-center gap-3 border-b border-line px-5",
        stall.live ? "hover:bg-card" : "opacity-60",
        justSeen && stall.live && "bg-clay/40",
      )}
    >
      <span className={cn("grid size-10 shrink-0 place-items-center rounded-xl", meta.wash)} aria-hidden>
        <Icon className="size-4" strokeWidth={1.75} />
      </span>
      {stall.live ? (
        <button
          type="button"
          disabled={busy}
          onClick={onOpen}
          className="min-w-0 flex-1 text-left disabled:cursor-wait"
        >
          <span className="block truncate text-[15px] font-semibold tracking-tight leading-tight">
            {stall.title}
          </span>
          <span className="mt-0.5 block h-5 truncate text-[12px] text-ink/75">
            {stallSubtitle(stall)}
          </span>
          <span className="mt-0.5 block truncate text-[11px] text-soft">{chips}</span>
        </button>
      ) : (
        <div className="min-w-0 flex-1">
          <span className="block truncate text-[15px] font-semibold tracking-tight leading-tight">
            {stall.title}
          </span>
          <span className="mt-0.5 block h-5 truncate text-[12px] text-ink/75">
            {stallSubtitle(stall)}
          </span>
          <span className="mt-0.5 block truncate text-[11px] text-soft">今晚还没开</span>
        </div>
      )}
      <button
        type="button"
        aria-label={stall.pinned ? "从常去拿下" : "标成常去"}
        onClick={() => togglePin(stall)}
        className="grid size-8 shrink-0 place-items-center rounded-full hover:bg-sidebar"
      >
        <Star
          className={cn("size-3.5", stall.pinned ? "fill-ember text-ember" : "text-soft")}
          strokeWidth={1.75}
        />
      </button>
    </div>
  );
});
