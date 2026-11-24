import { useRouteStore } from "../../stores/route";

export function RouteBanner() {
  const relayOnly = useRouteStore((state) => state.relayOnly);
  if (!relayOnly) {
    return null;
  }
  return (
    <div className="border-b border-ember/40 bg-clay px-5 py-3 text-sm leading-6 text-ember-dark">
      <p className="font-medium">直连没有打通，现在走中继</p>
      <p>数据还在绕远路，所以会慢。这通常是公司或家里的网关拦住了打洞用的 UDP，连接本身还在。</p>
    </div>
  );
}
