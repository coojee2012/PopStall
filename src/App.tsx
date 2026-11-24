import { isTauri } from "@tauri-apps/api/core";
import { Footprints, Search, Star, Store, Ticket, X } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useKept } from "./lib/memory";
import { PlazaView } from "./components/directory/PlazaView";
import { CodeDialog } from "./components/studio/CodeDialog";
import { HostPanel } from "./components/studio/HostPanel";
import { MyStallCard } from "./components/studio/MyStallCard";
import { RouteBanner } from "./components/studio/RouteBanner";
import { StageView } from "./components/studio/StageView";
import { TrafficMeter } from "./components/studio/TrafficMeter";
import { usePlazaStore, startPlaza } from "./stores/plaza";
import { startRoute } from "./stores/route";
import { useSession } from "./stores/session";
import { cn } from "./lib/utils";

type Desk = "market" | "kept" | "trail" | "stall";

export default function App() {
  const error = useSession((state) => state.error);
  const stage = useSession((state) => state.stage);
  const desktop = isTauri();
  const [desk, setDesk] = useState<Desk>("market");
  const [draft, setDraft] = useState("");
  const [query, setQuery] = useState("");
  const [codeOpen, setCodeOpen] = useState(false);
  const composing = useRef(false);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => startPlaza(), []);
  useEffect(() => startRoute(), []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA" || target?.isContentEditable;
      if (event.key === "/" && !typing && !codeOpen && desk !== "stall") {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [codeOpen, desk]);

  return (
    <main className="flex h-screen flex-col overflow-hidden">
      <header className="flex h-16 shrink-0 items-center gap-4 border-b border-line px-4 sm:px-5">
        <div className="flex shrink-0 items-center gap-3">
          <span className="grid size-10 place-items-center rounded-2xl border border-line bg-card font-serif text-lg text-ember">摊</span>
          <div className="flex items-baseline gap-2">
            <h1 className="font-serif text-3xl leading-none">闪摊</h1>
            <span className="hidden text-[0.62rem] tracking-[0.28em] text-ember sm:inline">POPSTALL</span>
          </div>
        </div>
        {desk === "stall" ? (
          <p className="min-w-0 flex-1 truncate font-serif text-2xl text-muted">摆摊</p>
        ) : (
          <label className="relative mx-1 min-w-0 flex-1">
            <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted" />
            <input
              ref={searchRef}
              value={draft}
              onChange={(event) => {
                const value = event.target.value;
                setDraft(value);
                if (!composing.current) {
                  setQuery(value);
                }
              }}
              onCompositionStart={() => {
                composing.current = true;
              }}
              onCompositionEnd={(event) => {
                composing.current = false;
                setQuery(event.currentTarget.value);
              }}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  setDraft("");
                  setQuery("");
                  event.currentTarget.blur();
                }
              }}
              placeholder="搜索摊名、一句话"
              aria-label="搜索摊名、一句话"
              className="w-full rounded-full border border-line bg-card py-2 pr-16 pl-9 text-sm outline-none focus:border-ember"
            />
            {draft ? (
              <button
                type="button"
                aria-label="清空搜索"
                onClick={() => {
                  setDraft("");
                  setQuery("");
                  searchRef.current?.focus();
                }}
                className="absolute top-1/2 right-3 grid size-6 -translate-y-1/2 place-items-center rounded-full text-muted hover:bg-paper"
              >
                <X className="size-3.5" />
              </button>
            ) : (
              <kbd className="pointer-events-none absolute top-1/2 right-3 hidden -translate-y-1/2 rounded-md border border-line px-1.5 font-mono text-[10px] text-muted sm:inline">
                /
              </kbd>
            )}
          </label>
        )}
        <OpenCount />
        {stage ? null : <TrafficMeter />}
      </header>

      {desktop ? null : (
        <p className="border-b border-line bg-clay px-5 py-2 text-sm leading-6 text-ember-dark">
          这是浏览器里的样子。选择文件夹和真正串门，要在闪摊窗口里。
        </p>
      )}
      {error ? <p className="border-b border-line bg-clay px-5 py-2 text-sm leading-6 text-ember-dark">{error}</p> : null}
      {stage ? null : <RouteBanner />}

      <div className="flex min-h-0 flex-1">
        <aside className="plaza-scroll flex min-h-0 w-[15.25rem] shrink-0 flex-col overflow-y-auto border-r border-line bg-card/60">
          <nav className="flex flex-col gap-1 px-3 pt-4">
            <NavButton active={desk === "market"} icon={<Store className="size-4" />} label="广场" onClick={() => setDesk("market")} />
            <NavButton active={desk === "kept"} icon={<Star className="size-4" />} label="常去" count={<KeptBadge />} onClick={() => setDesk("kept")} />
            <NavButton active={desk === "trail"} icon={<Footprints className="size-4" />} label="足迹" count={<TrailBadge />} onClick={() => setDesk("trail")} />
          </nav>
          <div className="mt-auto flex flex-col gap-3 px-3 pt-4 pb-4">
            <MyStallCard active={desk === "stall"} onOpen={() => setDesk("stall")} />
            <button
              type="button"
              onClick={() => setCodeOpen(true)}
              className="inline-flex items-center gap-2 rounded-full px-2 py-1.5 text-sm text-muted hover:text-ink"
            >
              <Ticket className="size-4" />
              有分享码
            </button>
          </div>
        </aside>
        {desk === "stall" ? (
          <div className="plaza-scroll min-h-0 min-w-0 flex-1 overflow-y-auto px-6 py-6">
            <div className="mx-auto w-full max-w-3xl">
              <HostPanel />
            </div>
          </div>
        ) : (
          <PlazaView desk={desk} query={query} />
        )}
      </div>
      {codeOpen ? <CodeDialog onClose={() => setCodeOpen(false)} /> : null}
      {stage ? <StageView /> : null}
    </main>
  );
}

function NavButton({
  active,
  icon,
  label,
  count,
  onClick,
}: {
  active: boolean;
  icon: ReactNode;
  label: string;
  count?: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-current={active ? "page" : undefined}
      onClick={onClick}
      className={cn(
        "flex items-center gap-2 rounded-2xl px-3 py-2 text-left text-sm transition",
        active ? "bg-card text-ink shadow-[0_8px_20px_rgba(36,25,16,0.04)]" : "text-muted hover:bg-card/70 hover:text-ink",
      )}
    >
      {icon}
      <span className="font-medium">{label}</span>
      {count ? <span className="ml-auto">{count}</span> : null}
    </button>
  );
}

function OpenCount() {
  const ready = usePlazaStore((state) => state.ready);
  const count = usePlazaStore((state) => state.stalls.length);
  return (
    <span className="shrink-0 font-mono text-xs text-muted tabular-nums">
      {ready ? `${count.toLocaleString("zh-CN")} 开着` : "接入中"}
    </span>
  );
}

function KeptBadge() {
  const kept = useKept();
  const stalls = usePlazaStore((state) => state.stalls);
  const pinned = kept.filter((row) => row.pinned);
  if (pinned.length === 0) {
    return null;
  }
  const live = new Set(stalls.map((stall) => stall.endpointId));
  const open = pinned.filter((row) => live.has(row.endpointId)).length;
  return (
    <span className="inline-flex items-center gap-1.5 font-mono text-xs text-muted tabular-nums">
      {open > 0 ? <span className="size-1.5 rounded-full bg-pine" /> : null}
      {pinned.length}
    </span>
  );
}

function TrailBadge() {
  const kept = useKept();
  const count = kept.filter((row) => row.visits > 0).length;
  if (count === 0) {
    return null;
  }
  return <span className="font-mono text-xs text-muted tabular-nums">{count}</span>;
}
