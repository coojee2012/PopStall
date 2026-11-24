import { isTauri } from "@tauri-apps/api/core";
import { Store } from "lucide-react";
import { useEffect, useState } from "react";
import { listPlaza } from "../../lib/api";
import { errorText } from "../../lib/utils";
import { useSession } from "../../stores/session";
import type { PlazaStall } from "../../types/ipc";
import { StatusDot } from "../common/StatusDot";
import { Card } from "../ui/card";

export function DirectoryPanel() {
  const desktop = isTauri();
  const busy = useSession((state) => state.guestBusy);
  const openDiscovered = useSession((state) => state.openDiscovered);
  const [stalls, setStalls] = useState<PlazaStall[]>([]);
  const [query, setQuery] = useState("");
  const [neighbors, setNeighbors] = useState(0);
  const [ready, setReady] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    if (!desktop) {
      return undefined;
    }
    let stopped = false;
    async function refresh() {
      try {
        const snapshot = await listPlaza();
        if (stopped) {
          return;
        }
        setStalls(snapshot.stalls);
        setNeighbors(snapshot.neighbors);
        setProblem(null);
        setReady(true);
      } catch (error) {
        if (!stopped) {
          setProblem(errorText(error));
          setReady(true);
        }
      }
    }
    void refresh();
    const timer = window.setInterval(() => {
      void refresh();
    }, 4000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [desktop]);

  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const visible = stalls.filter((stall) => {
    if (terms.length === 0) {
      return true;
    }
    const haystack = `${stall.title}\n${stall.label}\n${stall.blurb ?? ""}`.toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });

  const status = !desktop
    ? "在桌面窗口里才会开始找人"
    : !ready
      ? "正在接入广场"
      : neighbors > 0
        ? `已连上 ${neighbors} 个广场节点`
        : "正在找其他开着的闪摊";

  return (
    <Card>
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <p className="text-[0.68rem] tracking-[0.22em] text-muted">广场</p>
          <p className="mt-2 font-serif text-3xl">正在营业的摊</p>
        </div>
        <StatusDot tone={neighbors > 0 ? "live" : ready ? "busy" : "idle"} label={status} />
      </div>
      <p className="mt-3 max-w-3xl text-sm leading-6 text-muted">
        开着闪摊，就会进入同一张广场。别人开张之后出现在这里，点进去就是对方的页面。摊位内容仍然直接从对方电脑过来。
      </p>
      <input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder="搜索摊主写的词"
        className="mt-4 w-full rounded-full border border-line bg-paper px-4 py-2 text-sm outline-none focus:border-ember"
      />
      {problem ? <p className="mt-4 text-sm leading-6 text-ember-dark">{problem}</p> : null}
      {visible.length === 0 ? (
        <p className="mt-5 rounded-[1.3rem] bg-paper px-4 py-4 text-sm leading-6 text-muted">
          {stalls.length > 0
            ? "没有摊位带这些词。"
            : desktop
              ? "还没有看见别人的摊。两边都开着闪摊，局域网通常很快，跨网要等中继和广场入口接上。"
              : "用闪摊桌面窗口打开后，这里会列出别人正在营业的摊。"}
        </p>
      ) : (
        <div className="mt-5 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {visible.map((stall) => (
            <button
              key={stall.endpointId}
              type="button"
              disabled={busy}
              onClick={() => void openDiscovered(stall.endpointId, stall.title)}
              className="flex items-start gap-3 rounded-[1.3rem] border border-line bg-paper px-4 py-4 text-left transition hover:border-ember disabled:cursor-not-allowed disabled:opacity-50"
            >
              <Store className="mt-1 size-4 shrink-0 text-ember" />
              <span>
                <span className="block font-serif text-xl">{stall.title}</span>
                {stall.blurb ? <span className="mt-1 block text-sm leading-6 text-ink">{stall.blurb}</span> : null}
                <span className="mt-1 block text-xs text-muted">{stall.label || "摊位"}</span>
              </span>
            </button>
          ))}
        </div>
      )}
    </Card>
  );
}
