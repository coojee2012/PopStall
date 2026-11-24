import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { traffic } from "../../lib/api";
import { cn } from "../../lib/utils";

export function TrafficMeter({ floating = false }: { floating?: boolean }) {
  const desktop = isTauri();
  const [rates, setRates] = useState({ up: 0, down: 0 });

  useEffect(() => {
    if (!desktop) {
      return;
    }
    const history: { sent: number; received: number; at: number }[] = [];
    let alive = true;
    const tick = async () => {
      try {
        const sample = await traffic();
        const at = performance.now();
        history.push({ sent: sample.sent, received: sample.received, at });
        const cutoff = at - 4000;
        while (history.length > 1 && history[0].at < cutoff) {
          history.shift();
        }
        const first = history[0];
        const seconds = (at - first.at) / 1000;
        if (alive && seconds >= 0.8) {
          setRates({
            up: Math.max(0, (sample.sent - first.sent) / seconds),
            down: Math.max(0, (sample.received - first.received) / seconds),
          });
        }
      } catch {
        if (alive) {
          setRates({ up: 0, down: 0 });
        }
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 1000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [desktop]);

  return (
    <p
      className={cn(
        "pointer-events-none rounded-full border border-line bg-card/95 px-4 py-2 font-mono text-xs tracking-wide text-muted",
        floating && "fixed bottom-4 right-5 z-30 shadow-[0_10px_30px_rgba(36,25,16,0.08)]",
      )}
    >
      上传 {formatRate(rates.up)}
      <span className="mx-2 text-line">·</span>
      下载 {formatRate(rates.down)}
    </p>
  );
}

function formatRate(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec < 1) {
    return "0 B/s";
  }
  if (bytesPerSec < 1024) {
    return `${Math.round(bytesPerSec)} B/s`;
  }
  if (bytesPerSec < 1024 * 1024) {
    return `${(bytesPerSec / 1024).toFixed(1)} KB/s`;
  }
  return `${(bytesPerSec / (1024 * 1024)).toFixed(1)} MB/s`;
}
