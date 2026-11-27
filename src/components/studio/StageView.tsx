import { useEffect, useRef } from "react";
import { placeStage } from "../../lib/api";
import { useSession } from "../../stores/session";
import { StageChrome } from "../stage/StageChrome";

export function StageView() {
  const stage = useSession((state) => state.stage);
  const busy = useSession((state) => state.guestBusy);
  const closeStage = useSession((state) => state.closeStage);
  const chrome = useRef<HTMLDivElement>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!stage) {
      return;
    }
    let frame = 0;
    const place = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          const chromeEl = chrome.current;
          const top = Math.max(56, Math.ceil(chromeEl?.getBoundingClientRect().bottom ?? 56));
          const width = Math.max(1, window.innerWidth);
          const height = Math.max(1, window.innerHeight - top);
          void placeStage({ x: 0, y: top, width, height });
        });
      });
    };
    place();
    const observer = new ResizeObserver(place);
    if (chrome.current) {
      observer.observe(chrome.current);
    }
    if (root.current) {
      observer.observe(root.current);
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
    <div ref={root} className="fixed inset-0 z-40 flex h-screen w-screen flex-col overflow-hidden bg-paper">
      <StageChrome
        stage={stage}
        busy={busy}
        chromeRef={chrome}
        onBack={() => void closeStage()}
      />
      {/* Native child webview fills this region via place_stage */}
      <div className="min-h-0 flex-1 bg-paper" aria-hidden />
    </div>
  );
}
