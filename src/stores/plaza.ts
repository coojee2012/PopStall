import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import { listPlaza } from "../lib/api";
import { sameStalls } from "../lib/plaza";
import { errorText } from "../lib/utils";
import type { PlazaStall } from "../types/ipc";

const FRESH_MS = 15 * 60 * 1000;

type PlazaState = {
  stalls: PlazaStall[];
  ready: boolean;
  problem: string | null;
  freshUntil: Record<string, number>;
  introduced: boolean;
  apply: (stalls: PlazaStall[]) => void;
  fail: (message: string) => void;
};

export const usePlazaStore = create<PlazaState>((set, get) => ({
  stalls: [],
  ready: false,
  problem: null,
  freshUntil: {},
  introduced: false,
  apply(stalls) {
    const prev = get();
    if (prev.introduced && sameStalls(prev.stalls, stalls)) {
      return;
    }
    const now = Date.now();
    const freshUntil: Record<string, number> = {};
    for (const [id, until] of Object.entries(prev.freshUntil)) {
      if (until > now) {
        freshUntil[id] = until;
      }
    }
    if (prev.introduced) {
      const known = new Set(prev.stalls.map((stall) => stall.endpointId));
      for (const stall of stalls) {
        if (!known.has(stall.endpointId)) {
          freshUntil[stall.endpointId] = now + FRESH_MS;
        }
      }
    }
    set({ stalls, ready: true, problem: null, freshUntil, introduced: true });
  },
  fail(message) {
    set({ ready: true, problem: message });
  },
}));

let generation = 0;

export function startPlaza(): () => void {
  const token = ++generation;
  if (!isTauri()) {
    if (import.meta.env.DEV) {
      void import("../lib/previewStalls").then(({ previewStalls }) => {
        if (token !== generation) {
          return;
        }
        const stalls = previewStalls();
        const now = Date.now();
        const freshUntil: Record<string, number> = {};
        stalls.slice(0, 8).forEach((stall, index) => {
          freshUntil[stall.endpointId] = now + 16 * 60 * 1000 - index;
        });
        usePlazaStore.setState({ stalls, ready: true, problem: null, introduced: true, freshUntil });
      });
    } else {
      usePlazaStore.setState({ stalls: [], ready: true, problem: null, introduced: true, freshUntil: {} });
    }
    return () => {
      if (token === generation) {
        generation += 1;
      }
    };
  }

  const pull = async () => {
    try {
      const snapshot = await listPlaza();
      if (token !== generation) {
        return;
      }
      usePlazaStore.getState().apply(snapshot.stalls);
    } catch (error) {
      if (token !== generation) {
        return;
      }
      usePlazaStore.getState().fail(errorText(error));
    }
  };
  void pull();
  const timer = window.setInterval(() => void pull(), 4000);
  return () => {
    if (token === generation) {
      generation += 1;
    }
    window.clearInterval(timer);
  };
}
