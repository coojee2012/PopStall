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
  const stage = useSession((state) => state.stage);
  const guestId = stage?.kind === "guest" ? stage.endpointId ?? undefined : undefined;
  const showHelpers = (relayOnly || viaHelper) && (helperOffers > 0 || helpers.length > 0);

  if (!relayOnly && !viaHelper) {
    return null;
  }

  return (
    <div
      className={`border-b px-5 py-3 text-sm leading-6 ${
        viaHelper ? "border-pine/30 bg-moss/40 text-pine" : "border-ember/40 bg-clay text-ember-dark"
      }`}
    >
      {viaHelper ? (
        <>
          <p className="font-medium">这一趟走了互助中继</p>
          <p>{pathLabel ?? "直连帮手在转发"}。列表里不含 n0 公共中继，只显示愿意帮忙的节点。</p>
        </>
      ) : (
        <>
          <p className="font-medium">直连没有打通，现在走中继</p>
          <p>
            当前路径：{pathLabel ?? "公共中继"}。公共中继常被限速，所以影音会慢。
            {helperOffers > 0
              ? " 下面是发现的互助帮手（不含打洞用的公用中继）；约每 45 秒会自动试一次可用的。"
              : " 若有第三方打开「我来当中继」，会出现在列表里。摊主自己不能给自己的摊当桥。"}
          </p>
        </>
      )}

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
