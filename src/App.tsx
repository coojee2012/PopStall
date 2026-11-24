import { isTauri } from "@tauri-apps/api/core";
import { DirectoryPanel } from "./components/directory/DirectoryPanel";
import { GuestPanel } from "./components/studio/GuestPanel";
import { HostPanel } from "./components/studio/HostPanel";
import { StageView } from "./components/studio/StageView";
import { TrafficMeter } from "./components/studio/TrafficMeter";
import { useSession } from "./stores/session";

export default function App() {
  const error = useSession((state) => state.error);
  const host = useSession((state) => state.host);
  const stage = useSession((state) => state.stage);
  const desktop = isTauri();

  return (
    <main className="mx-auto flex min-h-screen max-w-6xl flex-col px-5 py-7 sm:px-8">
      <header className="flex flex-wrap items-end justify-between gap-6">
        <div className="flex items-center gap-4">
          <span className="grid size-12 place-items-center rounded-[1.15rem] border border-line bg-card font-serif text-xl text-ember shadow-[0_12px_30px_rgba(36,25,16,0.05)]">
            摊
          </span>
          <div>
            <p className="text-[0.68rem] tracking-[0.32em] text-ember">POPSTALL</p>
            <h1 className="mt-1 font-serif text-4xl leading-none tracking-tight">闪摊</h1>
          </div>
        </div>
        <p className="max-w-xs text-sm leading-6 text-muted sm:text-right">
          {host ? `${host.modeLabel}正在营业。关掉窗口，或者点打烊，摊位就收了。` : "开着这台电脑，摊位才在。不需要服务器。"}
        </p>
      </header>

      {desktop ? null : (
        <p className="mt-6 rounded-2xl border border-line bg-clay px-4 py-3 text-sm leading-6 text-ember-dark">
          闪摊是桌面应用，不是网页。请关掉这个页面，在项目目录执行 npm run tauri dev，然后使用弹出的「闪摊」窗口。
        </p>
      )}

      {error ? (
        <p className="mt-6 rounded-2xl border border-line bg-clay px-4 py-3 text-sm leading-6 text-ember-dark">{error}</p>
      ) : null}

      <div className="mt-8">
        <DirectoryPanel />
      </div>

      <div className="mt-5 grid items-start gap-5 lg:grid-cols-[minmax(0,1.25fr)_minmax(18rem,0.75fr)]">
        <HostPanel />
        <GuestPanel />
      </div>

      <footer className="mt-8 max-w-3xl text-xs leading-5 text-muted">
        广场上的摊可以直接进去。分享码则是点名找某一个摊。别人的页面和本机预览都开在闪摊这一页里，没有系统权限。
      </footer>
      {stage ? null : <TrafficMeter floating />}
      {stage ? <StageView /> : null}
    </main>
  );
}
