import { ArrowLeft } from "lucide-react";
import { useEffect, useRef } from "react";
import { placeStage } from "../../lib/api";
import { useSession } from "../../stores/session";
import { TrafficMeter } from "./TrafficMeter";

export function StageView() {
  const stage = useSession((state) => state.stage);
  const busy = useSession((state) => state.guestBusy);
  const closeStage = useSession((state) => state.closeStage);
  const slot = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = slot.current;
    if (!stage || !element) {
      return;
    }
    let frame = 0;
    const place = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const rect = element.getBoundingClientRect();
        if (rect.width < 2 || rect.height < 2) {
          return;
        }
        void placeStage({
          x: rect.left,
          y: rect.top,
          width: rect.width,
          height: rect.height,
        });
      });
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(element);
    window.addEventListener("resize", place);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener("resize", place);
    };
  }, [stage]);

  if (!stage) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-20 flex flex-col bg-paper">
      <header className="flex h-16 shrink-0 items-center gap-3 border-b border-line px-4">
        <button
          type="button"
          onClick={() => void closeStage()}
          disabled={busy}
          className="inline-flex items-center gap-2 rounded-full border border-line bg-card px-3 py-1.5 text-sm text-ink transition hover:bg-clay disabled:opacity-50"
        >
          <ArrowLeft className="size-4" />
          返回
        </button>
        <span className="grid size-9 place-items-center rounded-xl border border-line bg-card font-serif text-ember">摊</span>
        <div className="min-w-0">
          <p className="text-[0.68rem] tracking-[0.28em] text-ember">闪摊</p>
          <p className="truncate font-serif text-lg leading-tight">{stage.title}</p>
        </div>
        <div className="ml-auto flex items-center gap-3">
          <TrafficMeter />
          <p className="text-xs tracking-wide text-muted">{stage.kind === "guest" ? "串门" : "本机预览"}</p>
        </div>
      </header>
      <div ref={slot} className="min-h-0 flex-1 bg-paper" />
    </div>
  );
}
