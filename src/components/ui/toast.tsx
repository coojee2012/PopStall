import { useEffect, useState } from "react";
import { create } from "zustand";
import { cn } from "../../lib/utils";

type ToastItem = {
  id: number;
  message: string;
  tone: "ok" | "warn";
};

type ToastState = {
  items: ToastItem[];
  push: (message: string, tone?: "ok" | "warn") => void;
  dismiss: (id: number) => void;
};

let nextId = 1;

export const useToastStore = create<ToastState>((set) => ({
  items: [],
  push(message, tone = "ok") {
    const id = nextId++;
    set((state) => ({ items: [...state.items.slice(-2), { id, message, tone }] }));
    window.setTimeout(() => {
      set((state) => ({ items: state.items.filter((item) => item.id !== id) }));
    }, 2400);
  },
  dismiss(id) {
    set((state) => ({ items: state.items.filter((item) => item.id !== id) }));
  },
}));

export function toast(message: string, tone: "ok" | "warn" = "ok") {
  useToastStore.getState().push(message, tone);
}

export function ToastHost() {
  const items = useToastStore((state) => state.items);
  const dismiss = useToastStore((state) => state.dismiss);
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  if (!mounted || items.length === 0) {
    return null;
  }
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-5 z-[60] flex flex-col items-center gap-2 px-4">
      {items.map((item) => (
        <button
          key={item.id}
          type="button"
          onClick={() => dismiss(item.id)}
          className={cn(
            "toast-enter pointer-events-auto rounded-full border px-4 py-2 text-[13px] shadow-[0_10px_30px_rgba(28,25,23,0.12)]",
            item.tone === "ok"
              ? "border-line bg-card text-ink"
              : "border-ember/20 bg-clay text-ember-dark",
          )}
        >
          {item.message}
        </button>
      ))}
    </div>
  );
}
