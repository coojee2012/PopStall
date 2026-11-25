import { useRouteStore } from "../../stores/route";

export function RouteBanner() {
  const relayOnly = useRouteStore((state) => state.relayOnly);
  const viaHelper = useRouteStore((state) => state.viaHelper);
  const helperOffers = useRouteStore((state) => state.helperOffers);

  if (viaHelper) {
    return (
      <div className="border-b border-pine/30 bg-moss/40 px-5 py-3 text-sm leading-6 text-pine">
        <p className="font-medium">这一趟走了互助中继</p>
        <p>公共中继太慢时，改由附近愿意帮忙的节点转发。关掉舞台后，下一趟会重新判断。</p>
      </div>
    );
  }

  if (!relayOnly) {
    return null;
  }

  return (
    <div className="border-b border-ember/40 bg-clay px-5 py-3 text-sm leading-6 text-ember-dark">
      <p className="font-medium">直连没有打通，现在走中继</p>
      <p>
        数据还在绕远路，所以会慢。这通常是公司或家里的网关拦住了打洞用的 UDP，连接本身还在。
        {helperOffers > 0
          ? ` 附近有 ${helperOffers} 个互助中继，下一趟串门时会自动试更快的那条。`
          : " 若有人打开「我来当中继」，下一趟串门会自动试更快的互助转发。"}
      </p>
    </div>
  );
}
