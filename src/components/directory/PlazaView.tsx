import { LayoutGrid, List } from "lucide-react";
import { useMemo, useState } from "react";
import { Button } from "../ui/button";
import { useKept, type KeptStall } from "../../lib/memory";
import {
  kindFilters,
  matchesQuery,
  stallKind,
  type KindFilter,
} from "../../lib/plaza";
import { usePlazaStore } from "../../stores/plaza";
import { useSession } from "../../stores/session";
import type { PlazaStall } from "../../types/ipc";
import { cn } from "../../lib/utils";
import { VirtualList } from "./VirtualList";
import { StallCard, StallRow, type StallCardModel } from "../plaza/StallCard";
import { PlazaRail } from "../plaza/PlazaRail";

type Desk = "market" | "kept" | "trail";
type SortMode = "fresh" | "name";
type ViewMode = "grid" | "list";

type Entry = StallCardModel & {
  visits: number;
  lastVisit: number;
};

type Row = { id: string; tone: "note"; text: string } | { id: string; tone: "stall"; stall: Entry };

const STALL_HEIGHT = 88;
const NOTE_HEIGHT = 32;

export function PlazaView({
  desk,
  query,
  onOpenStall,
}: {
  desk: Desk;
  query: string;
  onOpenStall?: () => void;
}) {
  const stalls = usePlazaStore((state) => state.stalls);
  const ready = usePlazaStore((state) => state.ready);
  const problem = usePlazaStore((state) => state.problem);
  const freshUntil = usePlazaStore((state) => state.freshUntil);
  const kept = useKept();
  const busy = useSession((state) => state.guestBusy);
  const lastSeenId = useSession((state) => state.lastSeenId);
  const openDiscovered = useSession((state) => state.openDiscovered);
  const [kind, setKind] = useState<KindFilter>("all");
  const [sort, setSort] = useState<SortMode>("fresh");
  const [view, setView] = useState<ViewMode>("grid");
  const [ask, setAsk] = useState<Entry | null>(null);
  const [pass, setPass] = useState("");
  const [passError, setPassError] = useState<string | null>(null);

  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const entries = useMemo(() => buildEntries(stalls, kept, freshUntil), [stalls, kept, freshUntil]);
  const rows = useMemo(
    () => layoutRows(entries, desk, terms, kind, sort),
    [entries, desk, terms, kind, sort],
  );
  const stallRows = rows.filter((row): row is Extract<Row, { tone: "stall" }> => row.tone === "stall");
  const livePins = useMemo(
    () =>
      entries
        .filter((entry) => entry.live && entry.pinned)
        .sort((a, b) => a.title.localeCompare(b.title, "zh-CN")),
    [entries],
  );
  function beginOpen(stall: Entry) {
    if (!stall.live) {
      return;
    }
    if (stall.locked) {
      setAsk(stall);
      setPass("");
      setPassError(null);
      return;
    }
    void openDiscovered(stall.endpointId, stall.title, { label: stall.label, blurb: stall.blurb });
  }

  const trail = useMemo(
    () =>
      entries
        .filter((entry) => entry.visits > 0)
        .sort((a, b) => b.lastVisit - a.lastVisit)
        .slice(0, 6)
        .map((entry) => ({
          id: entry.endpointId,
          title: entry.title,
          label: entry.label,
          live: entry.live,
          onOpen: () => {
            if (!entry.live || busy) {
              return;
            }
            if (entry.locked) {
              setAsk(entry);
              setPass("");
              setPassError(null);
              return;
            }
            void openDiscovered(entry.endpointId, entry.title, {
              label: entry.label,
              blurb: entry.blurb,
            });
          },
        })),
    [entries, busy, openDiscovered],
  );
  const showBanner = desk === "market" && terms.length === 0 && kind === "all" && livePins.length > 0;
  const showRail = desk === "market" && view === "grid";

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-paper">
      <div className="flex gap-1.5 overflow-x-auto px-5 pt-4">
        {kindFilters.map((filter) => (
          <button
            key={filter.id}
            type="button"
            onClick={() => setKind(filter.id)}
            className={cn(
              "shrink-0 rounded-full px-3 py-1 text-[12px] font-medium transition",
              kind === filter.id
                ? "bg-ink text-paper"
                : "border border-line bg-card text-muted hover:text-ink",
            )}
          >
            {filter.label}
          </button>
        ))}
      </div>

      <div className="flex items-center justify-between gap-3 px-5 pt-3">
        <p className="text-[12px] text-muted tabular-nums">
          {ready ? `${stallRows.length.toLocaleString("zh-CN")} 个摊` : "正在接入广场"}
        </p>
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-0.5 text-[12px] text-muted">
            <SortButton active={sort === "fresh"} onClick={() => setSort("fresh")}>
              最新
            </SortButton>
            <span className="text-soft">/</span>
            <SortButton active={sort === "name"} onClick={() => setSort("name")}>
              名称
            </SortButton>
          </div>
          <div className="flex rounded-lg border border-line bg-card p-0.5">
            <ViewButton active={view === "grid"} label="卡片" onClick={() => setView("grid")}>
              <LayoutGrid className="size-3.5" strokeWidth={1.75} />
            </ViewButton>
            <ViewButton active={view === "list"} label="列表" onClick={() => setView("list")}>
              <List className="size-3.5" strokeWidth={1.75} />
            </ViewButton>
          </div>
        </div>
      </div>

      {problem ? <p className="px-5 pt-2 text-[13px] text-ember-dark">{problem}</p> : null}

      {showBanner ? (
        <div className="mx-5 mt-3 flex flex-wrap items-center gap-2 rounded-2xl border border-line bg-moss/80 px-4 py-2.5">
          <p className="text-[12px] font-medium text-pine">常去的摊今晚开着</p>
          {livePins.slice(0, 3).map((stall) => (
            <button
              key={stall.endpointId}
              type="button"
              disabled={busy}
              onClick={() => beginOpen(stall)}
              className="max-w-40 truncate rounded-full bg-card px-3 py-1 text-[12px] font-medium text-ink ring-1 ring-line disabled:opacity-50"
            >
              {stall.title}
            </button>
          ))}
          {livePins.length > 3 ? (
            <span className="text-[11px] text-pine">还有 {livePins.length - 3} 家</span>
          ) : null}
        </div>
      ) : null}

      <div className="mt-3 flex min-h-0 flex-1 gap-4 overflow-hidden px-5 pb-5">
        <div className="min-h-0 min-w-0 flex-1">
          {!ready ? (
            <p className="py-8 text-[13px] text-muted">正在把广场接进来。</p>
          ) : stallRows.length === 0 ? (
            <Empty
              desk={desk}
              filtered={terms.length > 0 || kind !== "all"}
              query={terms.join(" ")}
              onOpenStall={onOpenStall}
            />
          ) : view === "grid" ? (
            <div className="plaza-scroll h-full overflow-y-auto pr-1">
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 2xl:grid-cols-3">
                {stallRows.map((row) => (
                  <StallCard
                    key={row.stall.endpointId}
                    stall={row.stall}
                    busy={busy}
                    justSeen={row.stall.endpointId === lastSeenId}
                    onOpen={() => beginOpen(row.stall)}
                  />
                ))}
              </div>
            </div>
          ) : (
            <VirtualList
              items={rows}
              resetKey={`${desk}|${kind}|${sort}|${query}|list`}
              heightOf={(row) => (row.tone === "note" ? NOTE_HEIGHT : STALL_HEIGHT)}
              itemKey={(row) => row.id}
              renderItem={(row) =>
                row.tone === "note" ? (
                  <p className="flex h-full items-end px-0 pb-1 text-[11px] tracking-wide text-soft">
                    {row.text}
                  </p>
                ) : (
                  <StallRow
                    stall={row.stall}
                    busy={busy}
                    justSeen={row.stall.endpointId === lastSeenId}
                    onOpen={() => beginOpen(row.stall)}
                  />
                )
              }
            />
          )}
        </div>
        {showRail ? (
          <PlazaRail
            onPickKind={(next) => setKind(next)}
            onOpenStall={onOpenStall}
            trail={trail}
          />
        ) : null}
      </div>

      {ask ? (
        <div
          className="fixed inset-0 z-30 flex items-start justify-center bg-ink/25 px-4 pt-[14vh] backdrop-blur-[2px]"
          onMouseDown={() => setAsk(null)}
        >
          <form
            className="w-full max-w-md rounded-2xl border border-line bg-card px-6 py-5 shadow-[0_24px_60px_rgba(28,25,23,0.16)]"
            onMouseDown={(event) => event.stopPropagation()}
            onSubmit={(event) => {
              event.preventDefault();
              const stall = ask;
              const password = pass.trim();
              if (!password) {
                setPassError("先写密码");
                return;
              }
              setPassError(null);
              void openDiscovered(stall.endpointId, stall.title, {
                label: stall.label,
                blurb: stall.blurb,
                password,
              }).then((opened) => {
                if (opened) {
                  setAsk(null);
                  return;
                }
                setPassError(useSession.getState().error ?? "密码不对");
              });
            }}
          >
            <h2 className="text-xl font-semibold tracking-tight">进入 {ask.title}</h2>
            <p className="mt-2 text-[13px] leading-6 text-muted">
              这是私密摊位。广场上的那句话谁都看得见，里面的内容要密码。
            </p>
            <input
              type="password"
              value={pass}
              autoFocus
              autoComplete="off"
              maxLength={48}
              onChange={(event) => setPass(event.target.value)}
              className="mt-4 w-full rounded-full border border-line bg-sidebar px-4 py-2 text-[13px] outline-none focus:border-line-strong focus:bg-card"
            />
            {passError ? <p className="mt-3 text-[13px] text-ember-dark">{passError}</p> : null}
            <div className="mt-4 flex gap-2">
              <Button type="submit" disabled={busy || pass.trim().length === 0}>
                {busy ? "正在确认" : "进去"}
              </Button>
              <Button variant="quiet" type="button" onClick={() => setAsk(null)}>
                取消
              </Button>
            </div>
          </form>
        </div>
      ) : null}
    </section>
  );
}

function SortButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn("rounded-full px-2 py-1", active ? "font-medium text-ink" : "hover:text-ink")}
    >
      {children}
    </button>
  );
}

function ViewButton({
  active,
  label,
  onClick,
  children,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className={cn(
        "grid size-7 place-items-center rounded-md transition",
        active ? "bg-sidebar text-ink" : "text-soft hover:text-ink",
      )}
    >
      {children}
    </button>
  );
}

function Empty({
  desk,
  filtered,
  query,
  onOpenStall,
}: {
  desk: Desk;
  filtered: boolean;
  query: string;
  onOpenStall?: () => void;
}) {
  const copy = filtered
    ? query
      ? `没有摊位写着「${query}」。`
      : "这一类里还没有摊。"
    : desk === "kept"
      ? "点亮摊名旁边的星。下次打开闪摊，先看见他们今晚开没开。"
      : desk === "trail"
        ? "走进过的摊会留在这台电脑上，下次可以接着逛。"
        : "还没有看见别人的摊。对方也开着闪摊的话，过一会儿会出现在这里。";
  return (
    <div className="mt-2 max-w-xl rounded-2xl border border-line bg-card px-5 py-5 text-[13px] leading-6 text-muted shadow-[0_1px_2px_rgba(28,25,23,0.04)]">
      {copy}
      {desk === "market" && !filtered ? (
        <p className="mt-3 text-[12px] text-soft">
          也可以先自己开一间——顶栏右侧「我要摆摊」。
          <button
            type="button"
            onClick={() => onOpenStall?.()}
            className="ml-1 font-medium text-ink underline-offset-2 hover:underline"
          >
            去设置
          </button>
        </p>
      ) : null}
    </div>
  );
}

function buildEntries(
  stalls: PlazaStall[],
  kept: KeptStall[],
  freshUntil: Record<string, number>,
): Entry[] {
  const memory = new Map(kept.map((row) => [row.endpointId, row]));
  const liveIds = new Set(stalls.map((stall) => stall.endpointId));
  const live = stalls.map((stall) => {
    const saved = memory.get(stall.endpointId);
    return {
      ...stall,
      live: true,
      freshAt: freshUntil[stall.endpointId] ?? 0,
      pinned: saved?.pinned ?? false,
      visits: saved?.visits ?? 0,
      lastVisit: saved?.lastVisit ?? 0,
    };
  });
  const ghosts: Entry[] = kept
    .filter((row) => !liveIds.has(row.endpointId) && (row.pinned || row.visits > 0))
    .map((row) => ({
      endpointId: row.endpointId,
      title: row.title,
      label: row.label,
      blurb: row.blurb,
      locked: false,
      live: false,
      freshAt: 0,
      pinned: row.pinned,
      visits: row.visits,
      lastVisit: row.lastVisit,
    }));
  return [...live, ...ghosts];
}

function layoutRows(
  entries: Entry[],
  desk: Desk,
  terms: string[],
  kind: KindFilter,
  sort: SortMode,
): Row[] {
  const matched = entries.filter((entry) => {
    if (desk === "market" && !entry.live) {
      return false;
    }
    if (desk === "kept" && !entry.pinned) {
      return false;
    }
    if (desk === "trail" && entry.visits <= 0) {
      return false;
    }
    if (kind !== "all" && stallKind(entry.label) !== kind) {
      return false;
    }
    return matchesQuery(entry, terms);
  });
  const live = matched.filter((entry) => entry.live).sort((a, b) => compareLive(a, b, sort));
  const offline = matched
    .filter((entry) => !entry.live)
    .sort((a, b) => b.lastVisit - a.lastVisit || a.title.localeCompare(b.title, "zh-CN"));
  const rows: Row[] = [];
  if (live.length > 0 && offline.length > 0) {
    rows.push({ id: "note-live", tone: "note", text: "今晚开着" });
  }
  for (const stall of live) {
    rows.push({ id: stall.endpointId, tone: "stall", stall });
  }
  if (offline.length > 0) {
    rows.push({ id: "note-offline", tone: "note", text: "这会儿没开" });
  }
  for (const stall of offline) {
    rows.push({ id: stall.endpointId, tone: "stall", stall });
  }
  return rows;
}

function compareLive(a: Entry, b: Entry, sort: SortMode) {
  if (sort === "fresh" && a.freshAt !== b.freshAt) {
    return b.freshAt - a.freshAt;
  }
  return a.title.localeCompare(b.title, "zh-CN");
}
