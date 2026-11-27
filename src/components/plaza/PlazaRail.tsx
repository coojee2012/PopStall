import { BookOpen, FileText, Film, Headphones, Globe } from "lucide-react";
import type { StallKind } from "../../lib/plaza";
import { kindFilters, kindMeta } from "../../lib/plaza";
import { cn } from "../../lib/utils";

const HOT: StallKind[] = ["music", "novel", "video", "document", "site"];

export function PlazaRail({
  onPickKind,
  onOpenStall,
  trail,
}: {
  onPickKind: (kind: StallKind) => void;
  onOpenStall?: () => void;
  trail: { id: string; title: string; label: string; onOpen: () => void; live: boolean }[];
}) {
  return (
    <aside className="hidden w-[15.5rem] shrink-0 flex-col gap-4 xl:flex">
      <section className="rounded-2xl border border-line bg-card p-4 shadow-[0_1px_2px_rgba(28,25,23,0.04)]">
        <p className="text-[11px] font-medium tracking-wide text-ember">快速开摊</p>
        <p className="mt-1 text-[13px] leading-5 text-muted">
          顶栏右侧是唯一主入口。选文件夹或映射端口，分享码发出去就能串门。
        </p>
        <button
          type="button"
          onClick={() => onOpenStall?.()}
          className="mt-3 text-[12px] font-medium text-ink underline-offset-2 hover:underline"
        >
          去顶栏开摊 →
        </button>
      </section>

      <section className="rounded-2xl border border-line bg-card p-4 shadow-[0_1px_2px_rgba(28,25,23,0.04)]">
        <p className="text-[11px] font-medium tracking-wide text-muted">热门类型</p>
        <ul className="mt-2 flex flex-col gap-1">
          {HOT.map((id) => {
            const meta = kindMeta(id);
            const Icon = meta.Icon;
            const label = kindFilters.find((row) => row.id === id)?.label ?? meta.label;
            return (
              <li key={id}>
                <button
                  type="button"
                  onClick={() => onPickKind(id)}
                  className="flex w-full items-center gap-2.5 rounded-xl px-2 py-1.5 text-left text-[13px] text-ink transition hover:bg-sidebar"
                >
                  <span className={cn("grid size-7 place-items-center rounded-lg", meta.wash)}>
                    <Icon className="size-3.5" strokeWidth={1.75} />
                  </span>
                  {label}
                </button>
              </li>
            );
          })}
        </ul>
      </section>

      {trail.length > 0 ? (
        <section className="rounded-2xl border border-line bg-card p-4 shadow-[0_1px_2px_rgba(28,25,23,0.04)]">
          <p className="text-[11px] font-medium tracking-wide text-muted">最近足迹</p>
          <ul className="mt-2 flex flex-col gap-1">
            {trail.slice(0, 5).map((row) => (
              <li key={row.id}>
                <button
                  type="button"
                  disabled={!row.live}
                  onClick={row.onOpen}
                  className={cn(
                    "w-full truncate rounded-xl px-2 py-1.5 text-left text-[13px] transition",
                    row.live
                      ? "text-ink hover:bg-sidebar"
                      : "cursor-default text-soft",
                  )}
                >
                  {row.title}
                </button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <p className="px-1 text-[11px] leading-5 text-soft">
        <span className="inline-flex items-center gap-1">
          <Headphones className="size-3" /> 乐
        </span>
        {" · "}
        <span className="inline-flex items-center gap-1">
          <Film className="size-3" /> 影
        </span>
        {" · "}
        <span className="inline-flex items-center gap-1">
          <BookOpen className="size-3" /> 书
        </span>
        {" · "}
        <span className="inline-flex items-center gap-1">
          <FileText className="size-3" /> 档
        </span>
        {" · "}
        <span className="inline-flex items-center gap-1">
          <Globe className="size-3" /> 站
        </span>
      </p>
    </aside>
  );
}
