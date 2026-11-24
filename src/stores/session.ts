import { create } from "zustand";
import * as api from "../lib/api";
import { rememberVisit } from "../lib/memory";
import { errorText } from "../lib/utils";
import type { HostInfo, ScanReport, StageInfo, StallChoice, ThemeId } from "../types/ipc";

const themeOrder: ThemeId[] = ["image", "music", "video", "novel", "document"];

function preferredChoice(scan: ScanReport): StallChoice | null {
  const theme = themeOrder.find((item) => scan.counts[item] > 0);
  if (theme) {
    return theme;
  }
  return scan.hasIndex ? "site" : null;
}

type SessionState = {
  folder: string | null;
  scan: ScanReport | null;
  scanning: boolean;
  choice: StallChoice | null;
  port: string;
  blurb: string;
  host: HostInfo | null;
  openedAt: number | null;
  stage: StageInfo | null;
  lastSeenId: string | null;
  hostBusy: boolean;
  guestBusy: boolean;
  error: string | null;
  chooseFolder: () => Promise<void>;
  setChoice: (choice: StallChoice) => void;
  setPort: (port: string) => void;
  setBlurb: (blurb: string) => void;
  startHost: () => Promise<void>;
  stopHost: () => Promise<void>;
  openPreview: () => Promise<void>;
  openGuest: (shareCode: string) => Promise<void>;
  openDiscovered: (endpointId: string, title: string, detail?: { label?: string; blurb?: string }) => Promise<void>;
  refreshHost: () => Promise<void>;
  closeStage: () => Promise<void>;
};

export const useSession = create<SessionState>((set, get) => ({
  folder: null,
  scan: null,
  scanning: false,
  choice: null,
  port: "3000",
  blurb: "",
  host: null,
  openedAt: null,
  stage: null,
  lastSeenId: null,
  hostBusy: false,
  guestBusy: false,
  error: null,

  async chooseFolder() {
    set({ error: null });
    try {
      const folder = await api.pickFolder();
      if (!folder) {
        return;
      }
      set({ folder, scanning: true, scan: null, choice: null });
      const scan = await api.scanFolder(folder);
      set({ scan, scanning: false, choice: preferredChoice(scan) });
    } catch (error) {
      set({ error: errorText(error), scanning: false });
    }
  },

  setChoice(choice) {
    set({ choice, error: null });
  },

  setPort(port) {
    set({ port });
  },

  setBlurb(blurb) {
    set({ blurb: Array.from(blurb).slice(0, 100).join("") });
  },

  async startHost() {
    const { folder, choice, port, blurb } = get();
    if (!choice) {
      set({ error: "先选一种开张方式" });
      return;
    }
    if (choice === "app") {
      const parsed = Number(port);
      if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65535) {
        set({ error: "端口要是 1 到 65535 之间的整数" });
        return;
      }
      set({ hostBusy: true, error: null });
      try {
        const host = await api.startHost({ mode: "app", port: parsed, blurb });
        set({ host, openedAt: Date.now() });
      } catch (error) {
        set({ error: errorText(error) });
      } finally {
        set({ hostBusy: false });
      }
      return;
    }
    if (!folder) {
      set({ error: "先选择一个本地文件夹" });
      return;
    }
    set({ hostBusy: true, error: null });
    try {
      const host = await api.startHost({ mode: choice, root: folder, blurb });
      set({ host, openedAt: Date.now() });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ hostBusy: false });
    }
  },

  async stopHost() {
    set({ hostBusy: true, error: null });
    try {
      if (get().stage?.kind === "preview") {
        await api.closeStage();
        set({ stage: null });
      }
      await api.stopHost();
      set({ host: null, openedAt: null });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ hostBusy: false });
    }
  },

  async openPreview() {
    const host = get().host;
    if (!host) {
      set({ error: "先把摊位开起来" });
      return;
    }
    set({ guestBusy: true, error: null });
    try {
      const stage = await api.openPreview(host.localUrl, host.modeLabel);
      set({ stage });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ guestBusy: false });
    }
  },

  async openGuest(shareCode: string) {
    const trimmed = shareCode.trim();
    if (!trimmed) {
      set({ error: "先粘贴对方的分享码" });
      return;
    }
    set({ guestBusy: true, error: null });
    try {
      const stage = await api.openGuest(trimmed);
      if (stage.endpointId) {
        rememberVisit({ endpointId: stage.endpointId, title: stage.title || "对方的摊位" });
      }
      set({ stage, lastSeenId: stage.endpointId ?? null });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ guestBusy: false });
    }
  },

  async openDiscovered(endpointId: string, title: string, detail?: { label?: string; blurb?: string }) {
    set({ guestBusy: true, error: null });
    try {
      const stage = await api.openDiscovered(endpointId, title);
      rememberVisit({ endpointId, title, label: detail?.label, blurb: detail?.blurb });
      set({ stage, lastSeenId: endpointId });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ guestBusy: false });
    }
  },

  async refreshHost() {
    const current = get().host;
    if (!current) {
      return;
    }
    try {
      const host = await api.hostStatus();
      if (!host) {
        return;
      }
      if (host.shareCode !== current.shareCode || host.relayReady !== current.relayReady) {
        set({ host });
      }
    } catch {
      // Keep the share code already on screen.
    }
  },

  async closeStage() {
    set({ guestBusy: true, error: null });
    try {
      await api.closeStage();
      set({ stage: null });
    } catch (error) {
      set({ error: errorText(error) });
    } finally {
      set({ guestBusy: false });
    }
  },
}));
