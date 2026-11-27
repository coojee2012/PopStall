import { routeStatusNote } from "../../lib/utils";
import { useRouteStore, refreshHelpers, switchHelper } from "../../stores/route";
import { useSession } from "../../stores/session";
import { Button } from "../ui/button";

function shortId(id: string) {
  if (id.length <= 12) {
    return id;
  }
  return `${id.slice(0, 6)}…${id.slice(-4)}`;
}

export function RouteBanner() {
  const relayOnly = useRouteStore((state) => state.relayOnly);
  const viaHelper = useRouteStore((state) => state.viaHelper);
  const helperOffers = useRouteStore((state) => state.helperOffers);
  const pathLabel = useRouteStore((state) => state.pathLabel);
  const helpers = useRouteStore((state) => state.helpers);
  const probing = useRouteStore((state) => state.probing);
  const switching = useRouteStore((state) => state.switching);
  const probeError = useRouteStore((state) => state.probeError);
  const pathTrust = useRouteStore((state) => state.pathTrust);
  const digestVerified = useRouteStore((state) => state.digestVerified);
  const retryCount = useRouteStore((state) => state.retryCount);
  const degraded = useRouteStore((state) => state.degraded);
  const stage = useSession((state) => state.stage);
  const guestId = stage?.kind === "guest" ? stage.endpointId ?? undefined : undefined;
  const showHelpers = (relayOnly || viaHelper) && (helperOffers > 0 || helpers.length > 0);
  const status = routeStatusNote({
    pathTrust,
    pathLabel,
    viaHelper,
    relayOnly,
    digestVerified,
    retryCount,
    degraded,
  });

  if (!relayOnly && !viaHelper) {
    return null;
  }

  return (
    <div
      className={`border-b px-5 py-2.5 text-[12px] leading-5 ${
        viaHelper ? "border-pine/20 bg-moss text-pine" : "border-ember/25 bg-clay text-ember-dark"
      }`}
    >
      {viaHelper ? (
        <>
          <p className="font-medium">{status.label}</p>
          <p className="opacity-90">
            {pathLabel ?? "直连帮手在转发"}。正文在访客和摊主之间端到端加密，帮手只看得到必要的路由元数据。
            列表里不含 n0 公共中继。
          </p>
        </>
      ) : (
        <>
          <p className="font-medium">直连没有打通，现在走{status.label}</p>
          <p className="opacity-90">
            当前路径：{pathLabel ?? status.label}。公共中继常被限速，影音可能偏慢。
            {helperOffers > 0
              ? " 下方是可用的互助帮手。"
              : " 有第三方打开「我来当中继」时会出现在这里。"}
          </p>
        </>
      )}
      {status.warnings.length > 0 ? (
        <p className="mt-1 text-xs text-ember-dark">{status.warnings.join(" · ")}</p>
      ) : null}

      {showHelpers ? (
        <div className="mt-3 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="button"
              className="px-3 py-1.5 text-xs"
              disabled={probing || switching}
              onClick={() => void refreshHelpers(guestId)}
            >
              {probing ? "正在测延迟" : "刷新帮手"}
            </Button>
            {switching ? <span className="text-xs">正在改走互助中继…</span> : null}
          </div>
          <ul className="space-y-1.5">
            {helpers.map((item) => (
              <li key={item.endpointId} className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-line/70 bg-card/70 px-3 py-2 text-ink">
                <div className="min-w-0">
                  <p className="font-mono text-xs">{shortId(item.endpointId)}</p>
                  <p className="text-xs text-muted">
                    {item.note}
                    {item.rttMs != null ? ` · ${item.rttMs} ms` : ""}
                    {` · 限速 ${(item.limitBps / (1024 * 1024)).toFixed(item.limitBps % (1024 * 1024) === 0 ? 0 : 1)} MB/s`}
                  </p>
                </div>
                <Button
                  type="button"
                  className="px-3 py-1.5 text-xs"
                  disabled={!item.usable || switching}
                  onClick={() => void switchHelper(item.endpointId)}
                >
                  {item.usable ? "用这个" : "不可用"}
                </Button>
              </li>
            ))}
          </ul>
          {helpers.length === 0 && !probing ? (
            <p className="text-xs text-muted">还没有测到帮手。点刷新，或等对方打开互助中继。</p>
          ) : null}
          {probeError ? <p className="text-xs text-ember-dark">{probeError}</p> : null}
        </div>
      ) : null}
    </div>
  );
}
