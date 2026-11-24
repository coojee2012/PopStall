import { X } from "lucide-react";
import { useEffect } from "react";
import { GuestPanel } from "./GuestPanel";

export function CodeDialog({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-40 flex items-start justify-center bg-ink/30 px-4 pt-[12vh]" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="share-code-title"
        className="w-full max-w-lg rounded-3xl border border-line bg-card px-6 py-6 shadow-[0_24px_60px_rgba(36,25,16,0.16)]"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4">
          <div>
            <h2 id="share-code-title" className="font-serif text-2xl">
              用分享码找人
            </h2>
            <p className="mt-1 text-sm leading-6 text-muted">广场上找不到的时候，贴上对方单独给你的码。</p>
          </div>
          <button type="button" aria-label="关闭" onClick={onClose} className="grid size-8 place-items-center rounded-full hover:bg-paper">
            <X className="size-4" />
          </button>
        </div>
        <GuestPanel embedded />
      </div>
    </div>
  );
}
