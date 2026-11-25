import { create } from "zustand";
import { isTauri } from "@tauri-apps/api/core";
import * as api from "../lib/api";
import { errorText } from "../lib/utils";
import type { TalkLine } from "../types/ipc";

type TalkState = {
  lines: TalkLine[];
  ready: boolean;
  problem: string | null;
  send: (text: string) => Promise<boolean>;
};

export const useTalkStore = create<TalkState>((set) => ({
  lines: [],
  ready: false,
  problem: null,

  async send(text) {
    set({ problem: null });
    try {
      const snapshot = await api.talkSend(text);
      set({ lines: snapshot.lines, ready: true });
      return true;
    } catch (error) {
      set({ problem: errorText(error) });
      return false;
    }
  },
}));

let generation = 0;

export function startTalk() {
  if (!isTauri()) {
    return () => undefined;
  }
  const current = ++generation;
  let stop = false;
  const tick = async () => {
    if (stop || current !== generation) {
      return;
    }
    try {
      const snapshot = await api.talkSnapshot();
      if (!stop && current === generation) {
        useTalkStore.setState({ lines: snapshot.lines, ready: true });
      }
    } catch (error) {
      if (!stop && current === generation) {
        useTalkStore.setState({ problem: errorText(error) });
      }
    }
    if (!stop && current === generation) {
      window.setTimeout(() => void tick(), 1500);
    }
  };
  void tick();
  return () => {
    stop = true;
  };
}
