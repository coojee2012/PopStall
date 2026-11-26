import { ArrowLeft } from "lucide-react";
import { useEffect, useRef } from "react";
import { placeStage } from "../../lib/api";
import { useSession } from "../../stores/session";
import { RouteBanner } from "./RouteBanner";
import { TrafficMeter } from "./TrafficMeter";

export function StageView() {
  const stage = useSession((state) => state.stage);
  const busy = useSession((state) => state.guestBusy);
  const closeStage = useSession((state) => state.closeStage);
  const chrome = useRef<HTMLDivElement>(null);
  const slot = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const slotEl = slot.current;
    const chromeEl = chrome.current;
    if (!stage || !slotEl) {
      return;
    }
    let frame = 0;
    const place = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const rect = slotEl.getBoundingClientRect();
        if (rect.width < 2 || rect.height < 2) {
          return;
        }
        // Native child webview paints above the main page — never let it cover chrome.
        const chromeBottom = chromeEl?.getBoundingClientRect().bottom ?? rect.top;
        const top = Math.max(rect.top, chromeBottom, 48);
        void placeStage({
          x: Math.max(0, rect.left),
          y: top,
          width: rect.width,
          height: Math.max(1, rect.bottom - top),
        });
      });
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(slotEl);
    if (chromeEl) {
      observer.observe(chromeEl);
    }
    window.addEventListener("resize", place);
    document.addEventListener("fullscreenchange", place);
    window.addEventListener("orientationchange", place);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener("resize", place);
      document.removeEventListener("fullscreenchange", place);
      window.removeEventListener("orientationchange", place);
    };
  }, [stage]);

  if (!stage) {
    return null;
  }

  return (
    <div className="fixed inset-0 z-20 flex flex-col bg-paper">
      <div ref={chrome} className="shrink-0 bg-paper">
        <header className="flex h-14 items-center gap-2.5 border-b border-line px-3 sm:gap-3 sm:px-4">
          <button
            type="button"
            onClick={() => void closeStage()}
            disabled={busy}
            className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-line bg-card px-2.5 py-1.5 text-sm text-ink transition hover:bg-clay disabled:opacity-50 sm:px-3"
          >
            <ArrowLeft className="size-4" />
            返回
          </button>
          <div className="h-6 w-px shrink-0 bg-line" aria-hidden />
          <div className="min-w-0 flex-1">
            <p className="truncate font-serif text-base leading-tight sm:text-lg">{stage.title}</p>
            <p className="text-[0.68rem] tracking-[0.18em] text-muted">
              闪摊 · {stage.kind === "guest" ? "串门" : "本机预览"}
            </p>
          </div>
          <div className="ml-auto shrink-0">
            <TrafficMeter />
          </div>
        </header>
        <RouteBanner />
      </div>
      <div ref={slot} className="min-h-0 flex-1 bg-paper" />
    </div>
  );
}
