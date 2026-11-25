import { isTauri } from "@tauri-apps/api/core";
import { useEffect } from "react";
import { refreshHelpers, switchHelper, useRouteStore } from "../../stores/route";
import { useSession } from "../../stores/session";
import { Button } from "../ui/button";

function shortId(id: string) {
  if (id.length <= 12) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}

export function HelperDesk() {
  const desktop = isTauri();
  const helpers = useRouteStore((state) => state.helpers);
  const probing = useRouteStore((state) => state.probing);
  const switching = useRouteStore((state) => state.switching);
  const probeError = useRouteStore((state) => state.probeError);
  const helperOffers = useRouteStore((state) => state.helperOffers);
  const stage = useSession((state) => state.stage);
  const guestId = stage?.kind === "guest" ? stage.endpointId ?? undefined : undefined;
  const visiting = stage?.kind === "guest";

  useEffect(() => {
    if (!desktop) {
      return;
    }
    void refreshHelpers(guestId);
    const timer = window.setInterval(() => void refreshHelpers(guestId), 12_000);
    return () => window.clearInterval(timer);
  }, [desktop, guestId]);

  return (
    <section className="plaza-scroll flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto px-6 py-5">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-5">
        <header>
          <h2 className="font-serif text-3xl font-medium tracking-tight">互助</h2>
          <p className="mt-2 max-w-xl text-sm leading-6 text-muted">
            这里是别人打开「我来当中继」后，本机发现到的帮手。不含 n0 打洞用的公共中继。摊主不能给自己的摊当桥；要加速访问你家的摊，需要第三方帮手。
          </p>
        </header>

        <div className="flex flex-wrap items-center gap-3">
          <Button type="button" className="px-3 py-1.5" disabled={!desktop || probing} onClick={() => void refreshHelpers(guestId)}>
            {probing ? "正在测延迟" : "刷新并测延迟"}
          </Button>
          <span className="text-xs text-muted">
            {desktop ? `广播里约有 ${helperOffers} 个 · 列表 ${helpers.length} 个` : "浏览器里只是预览"}
          </span>
        </div>

        {!desktop ? (
          <p className="text-sm text-muted">真正发现帮手要在闪摊窗口里，两端都在线且互助话题能通上。</p>
        ) : null}

        {helpers.length === 0 ? (
          <div className="rounded-[1.3rem] border border-dashed border-line px-5 py-10 text-center">
            <p className="font-serif text-2xl">还没发现帮手</p>
            <p className="mt-2 text-sm leading-6 text-muted">
              对方需打开「摆摊 → 我来当中继」，且两边广场/gossip 能互相找到。若你正在访问的就是对方自己的摊，对方即使开了中继也不会出现在「可用」里。
            </p>
          </div>
        ) : (
          <ul className="flex flex-col gap-2">
            {helpers.map((item) => (
              <li
                key={item.endpointId}
                className="flex flex-wrap items-center justify-between gap-3 rounded-[1.2rem] border border-line bg-card px-4 py-3"
              >
                <div className="min-w-0">
                  <p className="font-mono text-sm">{shortId(item.endpointId)}</p>
                  <p className="mt-1 text-xs leading-5 text-muted">
                    {item.note}
                    {item.rttMs != null ? ` · ${item.rttMs} ms` : ""}
                    {` · 限速 ${(item.limitBps / (1024 * 1024)).toFixed(item.limitBps % (1024 * 1024) === 0 ? 0 : 1)} MB/s`}
                    {item.load > 0 ? ` · 忙 ${item.load}` : ""}
                  </p>
                </div>
                <Button
                  type="button"
                  className="px-3 py-1.5 text-xs"
                  disabled={!visiting || !item.usable || switching}
                  onClick={() => void switchHelper(item.endpointId)}
                >
                  {!visiting ? "先打开一间摊" : item.usable ? "用这个转发" : "不可用"}
                </Button>
              </li>
            ))}
          </ul>
        )}

        {probeError ? <p className="text-sm text-ember-dark">{probeError}</p> : null}
      </div>
    </section>
  );
}
