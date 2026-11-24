import { useSyncExternalStore } from "react";

const KEY = "popstall.kept.v1";
const MAX_TRAIL = 300;

export type KeptStall = {
  endpointId: string;
  title: string;
  label: string;
  blurb: string;
  visits: number;
  lastVisit: number;
  pinned: boolean;
};

type StallSeed = {
  endpointId: string;
  title: string;
  label?: string;
  blurb?: string;
};

const listeners = new Set<() => void>();
let snapshot: KeptStall[] = readStorage();

function readStorage(): KeptStall[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) {
      return [];
    }
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter(isKept);
  } catch {
    return [];
  }
}

function isKept(value: unknown): value is KeptStall {
  if (!value || typeof value !== "object") {
    return false;
  }
  const row = value as Partial<KeptStall>;
  return typeof row.endpointId === "string" && row.endpointId.length > 0 && typeof row.title === "string";
}

function commit(rows: KeptStall[]) {
  snapshot = prune(rows);
  try {
    localStorage.setItem(KEY, JSON.stringify(snapshot));
  } catch {
    // The plaza still works if the browser refuses to store the trail.
  }
  listeners.forEach((listener) => listener());
}

function prune(rows: KeptStall[]): KeptStall[] {
  const pinned = rows.filter((row) => row.pinned);
  const trail = rows
    .filter((row) => !row.pinned && row.visits > 0)
    .sort((a, b) => b.lastVisit - a.lastVisit)
    .slice(0, MAX_TRAIL);
  return [...pinned, ...trail];
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useKept() {
  return useSyncExternalStore(subscribe, () => snapshot, () => snapshot);
}

export function rememberVisit(seed: StallSeed) {
  const prev = snapshot.find((row) => row.endpointId === seed.endpointId);
  const next: KeptStall = {
    endpointId: seed.endpointId,
    title: seed.title || prev?.title || "摊位",
    label: seed.label || prev?.label || "",
    blurb: seed.blurb || prev?.blurb || "",
    visits: (prev?.visits ?? 0) + 1,
    lastVisit: Date.now(),
    pinned: prev?.pinned ?? false,
  };
  commit([next, ...snapshot.filter((row) => row.endpointId !== seed.endpointId)]);
}

export function togglePin(seed: StallSeed) {
  const prev = snapshot.find((row) => row.endpointId === seed.endpointId);
  const pinned = !prev?.pinned;
  const next: KeptStall = {
    endpointId: seed.endpointId,
    title: seed.title || prev?.title || "摊位",
    label: seed.label || prev?.label || "",
    blurb: seed.blurb || prev?.blurb || "",
    visits: prev?.visits ?? 0,
    lastVisit: prev?.lastVisit ?? 0,
    pinned,
  };
  const rest = snapshot.filter((row) => row.endpointId !== seed.endpointId);
  if (!pinned && next.visits === 0) {
    commit(rest);
    return;
  }
  commit([next, ...rest]);
}
