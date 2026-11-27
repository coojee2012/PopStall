import { isTauri } from "@tauri-apps/api/core";
import {
  Footprints,
  MessageCircle,
  Plus,
  Search,
  Share2,
  Star,
  Store,
  Ticket,
  X,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useKept } from "./lib/memory";
import { PlazaView } from "./components/directory/PlazaView";
import { CodeDialog } from "./components/studio/CodeDialog";
import { HelperDesk } from "./components/studio/HelperDesk";
import { HostPanel } from "./components/studio/HostPanel";
import { MyStallCard } from "./components/studio/MyStallCard";
import { RouteBanner } from "./components/studio/RouteBanner";
import { StageView } from "./components/studio/StageView";
import { StatusCapsule } from "./components/studio/StatusCapsule";
import { TalkRoom } from "./components/talk/TalkRoom";
import { ToastHost } from "./components/ui/toast";
import { usePlazaStore, startPlaza } from "./stores/plaza";
import { startRoute } from "./stores/route";
import { startTalk, useTalkStore } from "./stores/talk";
import { useSession } from "./stores/session";
import { cn } from "./lib/utils";

type Desk = "market" | "kept" | "trail" | "talk" | "help" | "stall";

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
  useEffect(() => startTalk(), []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing =
        target?.tagName === "INPUT" ||
        target?.tagName === "TEXTAREA" ||
        target?.isContentEditable;
      if (
        event.key === "/" &&
        !typing &&
        !codeOpen &&
        desk !== "stall" &&
        desk !== "talk" &&
        desk !== "help"
      ) {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [codeOpen, desk]);

  const browsing = desk === "market" || desk === "kept" || desk === "trail";

  return (
    <main className="flex h-screen w-screen flex-col overflow-hidden bg-paper text-ink">
      <header className="relative z-10 flex h-14 shrink-0 items-center gap-3 border-b border-line bg-card/90 px-4 backdrop-blur-md sm:px-5">
        <div className="flex shrink-0 items-center gap-2.5">
          <span className="grid size-8 place-items-center rounded-xl border border-line bg-sidebar text-[13px] font-semibold tracking-tight text-ember">
            摊
          </span>
          <h1 className="text-[15px] font-semibold tracking-tight">闪摊</h1>
        </div>

        {browsing ? (
          <label className="relative mx-auto w-full max-w-md min-w-0">
            <Search className="pointer-events-none absolute top-1/2 left-3 size-3.5 -translate-y-1/2 text-soft" strokeWidth={1.75} />
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
              placeholder="搜索摊名、一句话或标签"
              aria-label="搜索摊名、一句话或标签"
              className="w-full rounded-full border border-line bg-sidebar py-2 pr-14 pl-9 text-[13px] outline-none transition placeholder:text-soft focus:border-line-strong focus:bg-card"
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
                className="absolute top-1/2 right-2.5 grid size-6 -translate-y-1/2 place-items-center rounded-full text-muted hover:bg-card"
              >
                <X className="size-3.5" strokeWidth={1.75} />
              </button>
            ) : (
              <kbd className="pointer-events-none absolute top-1/2 right-3 hidden -translate-y-1/2 rounded-md border border-line bg-card px-1.5 font-mono text-[10px] text-soft sm:inline">
                /
              </kbd>
            )}
          </label>
        ) : (
          <p className="mx-auto min-w-0 flex-1 truncate text-center text-[15px] font-medium text-muted">
            {desk === "stall" ? "摆摊" : desk === "talk" ? "夜谈" : "互助"}
          </p>
        )}

        <div className="ml-auto flex shrink-0 items-center gap-2">
          <StatusCapsule />
          <button
            type="button"
            onClick={() => setDesk("stall")}
            className="inline-flex items-center gap-1.5 rounded-full bg-ember px-3 py-1.5 text-[12px] font-medium text-white shadow-[0_8px_18px_rgba(180,83,9,0.2)] transition hover:bg-ember-dark"
          >
            <Plus className="size-3.5" strokeWidth={2} />
            <span className="hidden sm:inline">我要摆摊</span>
            <span className="sm:hidden">摆摊</span>
          </button>
        </div>
      </header>

      {desktop ? null : (
        <p className="border-b border-line bg-clay px-5 py-2 text-[13px] leading-6 text-ember-dark">
          这是浏览器预览。选文件夹和真正串门，请在闪摊窗口里操作。
        </p>
      )}
      {error ? (
        <p className="border-b border-line bg-clay px-5 py-2 text-[13px] leading-6 text-ember-dark">{error}</p>
      ) : null}
      {stage ? null : <RouteBanner />}

      <div className="flex min-h-0 flex-1">
        <aside className="plaza-scroll flex min-h-0 w-[15.5rem] shrink-0 flex-col overflow-y-auto border-r border-line bg-sidebar">
          <nav className="flex flex-col gap-0.5 px-2.5 pt-3">
            <NavButton
              active={desk === "market"}
              icon={<Store className="size-4" strokeWidth={1.5} />}
              label="广场"
              onClick={() => setDesk("market")}
            />
            <NavButton
              active={desk === "kept"}
              icon={<Star className="size-4" strokeWidth={1.5} />}
              label="常去"
              count={<KeptBadge />}
              onClick={() => setDesk("kept")}
            />
            <NavButton
              active={desk === "trail"}
              icon={<Footprints className="size-4" strokeWidth={1.5} />}
              label="足迹"
              count={<TrailBadge />}
              onClick={() => setDesk("trail")}
            />
            <NavButton
              active={desk === "talk"}
              icon={<MessageCircle className="size-4" strokeWidth={1.5} />}
              label="夜谈"
              count={<TalkBadge active={desk === "talk"} />}
              onClick={() => setDesk("talk")}
            />
            <NavButton
              active={desk === "help"}
              icon={<Share2 className="size-4" strokeWidth={1.5} />}
              label="互助"
              onClick={() => setDesk("help")}
            />
          </nav>
          <div className="mt-auto flex flex-col gap-2 px-2.5 pt-4 pb-4">
            <MyStallCard active={desk === "stall"} onOpen={() => setDesk("stall")} />
            <button
              type="button"
              onClick={() => setCodeOpen(true)}
              className="inline-flex items-center gap-2 rounded-xl px-2.5 py-2 text-[12px] text-muted transition hover:bg-card hover:text-ink"
            >
              <Ticket className="size-3.5" strokeWidth={1.5} />
              有分享码
            </button>
          </div>
        </aside>
        {desk === "stall" ? (
          <div className="plaza-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-paper px-6 py-6">
            <div className="mx-auto w-full max-w-3xl">
              <HostPanel />
            </div>
          </div>
        ) : desk === "talk" ? (
          <TalkRoom />
        ) : desk === "help" ? (
          <HelperDesk />
        ) : (
          <PlazaView desk={desk} query={query} onOpenStall={() => setDesk("stall")} />
        )}
      </div>
      {codeOpen ? <CodeDialog onClose={() => setCodeOpen(false)} /> : null}
      {stage ? <StageView /> : null}
      <ToastHost />
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
        "flex h-9 items-center gap-2.5 rounded-xl px-2.5 text-left text-[13px] transition",
        active
          ? "bg-card font-medium text-ink shadow-[0_1px_2px_rgba(28,25,23,0.06)] ring-1 ring-line"
          : "text-muted hover:bg-card/70 hover:text-ink",
      )}
    >
      <span className={cn(active ? "text-ember" : "text-soft")}>{icon}</span>
      <span>{label}</span>
      {count ? <span className="ml-auto">{count}</span> : null}
    </button>
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
    <span className="inline-flex items-center gap-1.5 font-mono text-[11px] text-muted tabular-nums">
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
  return <span className="font-mono text-[11px] text-muted tabular-nums">{count}</span>;
}

function TalkBadge({ active }: { active: boolean }) {
  const count = useTalkStore((state) => state.lines.length);
  const seen = useRef(0);
  if (active) {
    seen.current = count;
  }
  if (active || count <= seen.current) {
    return null;
  }
  return <span className="size-1.5 rounded-full bg-ember" />;
}
