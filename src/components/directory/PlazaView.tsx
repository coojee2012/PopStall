import { Star } from "lucide-react";
import { memo, useMemo, useState } from "react";
import { Button } from "../ui/button";
import { useKept, togglePin, type KeptStall } from "../../lib/memory";
import { kindFilters, kindGlyph, kindName, kindWash, matchesQuery, stallKind, type KindFilter } from "../../lib/plaza";
import { usePlazaStore } from "../../stores/plaza";
import { useSession } from "../../stores/session";
import type { PlazaStall } from "../../types/ipc";
import { cn } from "../../lib/utils";
import { VirtualList } from "./VirtualList";

type Desk = "market" | "kept" | "trail";
type SortMode = "fresh" | "name";

type Entry = PlazaStall & {
  live: boolean;
  freshAt: number;
  pinned: boolean;
  visits: number;
  lastVisit: number;
};

type Row = { id: string; tone: "note"; text: string } | { id: string; tone: "stall"; stall: Entry };

const STALL_HEIGHT = 88;
const NOTE_HEIGHT = 36;

export function PlazaView({ desk, query }: { desk: Desk; query: string }) {
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
  const [ask, setAsk] = useState<Entry | null>(null);
  const [pass, setPass] = useState("");
  const [passError, setPassError] = useState<string | null>(null);

  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const entries = useMemo(() => buildEntries(stalls, kept, freshUntil), [stalls, kept, freshUntil]);
  const rows = useMemo(() => layoutRows(entries, desk, terms, kind, sort), [entries, desk, terms, kind, sort]);
  const livePins = useMemo(
    () => entries.filter((entry) => entry.live && entry.pinned).sort((a, b) => a.title.localeCompare(b.title, "zh-CN")),
    [entries],
  );

  const showBanner = desk === "market" && terms.length === 0 && kind === "all" && livePins.length > 0;

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

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex gap-1.5 overflow-x-auto px-5 pt-4">
        {kindFilters.map((filter) => (
          <button
            key={filter.id}
            type="button"
            onClick={() => setKind(filter.id)}
            className={cn(
              "shrink-0 rounded-full px-3 py-1 text-sm transition",
              kind === filter.id ? "bg-ink text-paper" : "border border-line bg-card text-muted hover:text-ink",
            )}
          >
            {filter.label}
          </button>
        ))}
      </div>
      <div className="flex items-center justify-between gap-3 px-5 pt-3">
        <p className="text-sm text-muted">
          {ready ? `${rows.filter((row) => row.tone === "stall").length.toLocaleString("zh-CN")} 个摊` : "正在接入广场"}
        </p>
        <div className="flex items-center gap-1 text-xs text-muted">
          <SortButton active={sort === "fresh"} onClick={() => setSort("fresh")}>
            最新
          </SortButton>
          <span className="text-line">/</span>
          <SortButton active={sort === "name"} onClick={() => setSort("name")}>
            名称
          </SortButton>
        </div>
      </div>
      {problem ? <p className="px-5 pt-2 text-sm text-ember-dark">{problem}</p> : null}
      {showBanner ? (
        <div className="mx-5 mt-3 flex flex-wrap items-center gap-2 rounded-2xl bg-moss px-4 py-3">
          <p className="text-sm text-pine">常去的摊今晚开着</p>
          {livePins.slice(0, 3).map((stall) => (
            <button
              key={stall.endpointId}
              type="button"
              disabled={busy}
              onClick={() => beginOpen(stall)}
              className="max-w-40 truncate rounded-full bg-card px-3 py-1 font-serif text-sm text-ink disabled:opacity-50"
            >
              {stall.title}
            </button>
          ))}
          {livePins.length > 3 ? <span className="text-xs text-pine">还有 {livePins.length - 3} 家</span> : null}
        </div>
      ) : null}
      <div className="mt-2 min-h-0 flex-1">
        {!ready ? (
          <p className="px-5 py-8 text-sm text-muted">正在把广场接进来。</p>
        ) : rows.length === 0 ? (
          <Empty desk={desk} filtered={terms.length > 0 || kind !== "all"} query={terms.join(" ")} />
        ) : (
          <VirtualList
            items={rows}
            resetKey={`${desk}|${kind}|${sort}|${query}`}
            heightOf={(row) => (row.tone === "note" ? NOTE_HEIGHT : STALL_HEIGHT)}
            itemKey={(row) => row.id}
            renderItem={(row) =>
              row.tone === "note" ? (
                <p className="flex h-full items-end px-5 pb-1 text-xs tracking-wide text-muted">{row.text}</p>
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
      {ask ? (
        <div className="fixed inset-0 z-30 flex items-start justify-center bg-ink/30 px-4 pt-[14vh]" onMouseDown={() => setAsk(null)}>
          <form
            className="w-full max-w-md rounded-3xl border border-line bg-card px-6 py-6 shadow-[0_24px_60px_rgba(36,25,16,0.16)]"
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
            <h2 className="font-serif text-2xl">进入{ask.title}</h2>
            <p className="mt-2 text-sm leading-6 text-muted">这是私密摊位。广场上的那句话谁都看得见，里面的内容要密码。</p>
            <input
              type="password"
              value={pass}
              autoFocus
              autoComplete="off"
              maxLength={48}
              onChange={(event) => setPass(event.target.value)}
              className="mt-4 w-full rounded-full border border-line bg-paper px-4 py-2 text-sm outline-none focus:border-ember"
            />
            {passError ? <p className="mt-3 text-sm text-ember-dark">{passError}</p> : null}
            <div className="mt-4 flex gap-3">
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

function SortButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: string }) {
  return (
    <button type="button" onClick={onClick} className={cn("rounded-full px-2 py-1", active ? "text-ink" : "hover:text-ink")}>
      {children}
    </button>
  );
}

function Empty({ desk, filtered, query }: { desk: Desk; filtered: boolean; query: string }) {
  const copy = filtered
    ? query
      ? `没有摊位写着「${query}」。`
      : "这一类里还没有摊。"
    : desk === "kept"
      ? "点亮摊名旁边的星。下次打开闪摊，先看见他们今晚开没开。"
      : desk === "trail"
        ? "走进过的摊会留在这台电脑上，下次可以接着逛。"
        : "还没有看见别人的摊。对方也开着闪摊的话，过一会儿会出现在这里。";
  return <p className="mx-5 mt-4 rounded-2xl bg-card px-4 py-4 text-sm leading-6 text-muted">{copy}</p>;
}

const StallRow = memo(function StallRow({
  stall,
  busy,
  justSeen,
  onOpen,
}: {
  stall: Entry;
  busy: boolean;
  justSeen: boolean;
  onOpen: () => void;
}) {
  const kind = stallKind(stall.label);
  const fresh = stall.live && stall.freshAt > Date.now();
  const meta = [stall.label || kindName(kind), stall.locked ? "要密码" : null, fresh ? "刚出现" : null, justSeen ? "刚刚看过" : null]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      className={cn(
        "flex h-full items-center gap-3 border-b border-line/80 px-5",
        stall.live ? "hover:bg-card" : "opacity-60",
        justSeen && stall.live && "bg-clay/50",
      )}
    >
      <span className={cn("grid size-11 shrink-0 place-items-center rounded-2xl font-serif text-lg", kindWash(kind))}>{kindGlyph(kind)}</span>
      {stall.live ? (
        <button
          type="button"
          disabled={busy}
          onClick={onOpen}
          className="min-w-0 flex-1 text-left disabled:cursor-wait"
        >
          <StallCopy title={stall.title} blurb={stall.blurb} meta={meta} />
        </button>
      ) : (
        <div className="min-w-0 flex-1">
          <StallCopy title={stall.title} blurb={stall.blurb} meta="今晚还没开" />
        </div>
      )}
      <button
        type="button"
        aria-label={stall.pinned ? "从常去拿下" : "标成常去"}
        onClick={() => togglePin(stall)}
        className="grid size-9 shrink-0 place-items-center rounded-full hover:bg-paper"
      >
        <Star className={cn("size-4", stall.pinned ? "fill-ember text-ember" : "text-muted")} />
      </button>
    </div>
  );
});

function StallCopy({ title, blurb, meta }: { title: string; blurb: string; meta: string }) {
  return (
    <>
      <span className="block truncate font-serif text-[1.15rem] leading-tight">{title}</span>
      <span className="mt-0.5 block h-5 truncate text-sm text-ink/80">{blurb}</span>
      <span className="mt-0.5 block truncate text-xs text-muted">{meta}</span>
    </>
  );
}

function buildEntries(stalls: PlazaStall[], kept: KeptStall[], freshUntil: Record<string, number>): Entry[] {
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

function layoutRows(entries: Entry[], desk: Desk, terms: string[], kind: KindFilter, sort: SortMode): Row[] {
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
  const offline = matched.filter((entry) => !entry.live).sort((a, b) => b.lastVisit - a.lastVisit || a.title.localeCompare(b.title, "zh-CN"));
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
