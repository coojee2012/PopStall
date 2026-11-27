import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import { helperList, routeStatus, useHelper } from "../lib/api";
import type { HelperOffer, PathTrustValue } from "../types/ipc";
import { useSession } from "./session";

type RouteState = {
  relayOnly: boolean;
  viaHelper: boolean;
  helperOffers: number;
  pathLabel: string | null;
  helpers: HelperOffer[];
  probing: boolean;
  switching: boolean;
  probeError: string | null;
  pathTrust: PathTrustValue;
  digestVerified: boolean;
  retryCount: number;
  degraded: boolean;
};

export const useRouteStore = create<RouteState>(() => ({
  relayOnly: false,
  viaHelper: false,
  helperOffers: 0,
  pathLabel: null,
  helpers: [],
  probing: false,
  switching: false,
  probeError: null,
  pathTrust: null,
  digestVerified: false,
  retryCount: 0,
  degraded: true,
}));

const AUTO_INTERVAL_MS = 45_000;
const HELPER_REFRESH_GAP_MS = 20_000;
let generation = 0;
let lastAuto = 0;
let lastHelperRefresh = 0;
let helperRefreshInFlight: Promise<void> | null = null;

export function startRoute(): () => void {
  const token = ++generation;
  if (!isTauri()) {
    useRouteStore.setState({
      relayOnly: false,
      viaHelper: false,
      helperOffers: 0,
      pathLabel: null,
      helpers: [],
      probing: false,
      switching: false,
      probeError: null,
      pathTrust: null,
      digestVerified: false,
      retryCount: 0,
      degraded: true,
    });
    return () => {
      if (token === generation) {
        generation += 1;
      }
    };
  }
  const pull = async () => {
    try {
      const status = await routeStatus();
      if (token !== generation) {
        return;
      }
      useRouteStore.setState({
        relayOnly: status.relayOnly,
        viaHelper: status.viaHelper,
        helperOffers: status.helperOffers,
        pathLabel: status.pathLabel ?? null,
        pathTrust: status.pathTrust ?? null,
        digestVerified: status.digestVerified ?? false,
        retryCount: status.retryCount ?? 0,
        degraded: status.degraded ?? true,
      });
      const stage = useSession.getState().stage;
      const guestId = stage?.kind === "guest" ? stage.endpointId : null;
      // Keep the 互助 list populated, but not every 2s — stacked probes abort
      // hole-punch before a direct path can win.
      const now = Date.now();
      if (
        (status.helperOffers > 0 || (status.relayOnly && guestId)) &&
        now - lastHelperRefresh >= HELPER_REFRESH_GAP_MS
      ) {
        lastHelperRefresh = now;
        await refreshHelpers(guestId ?? undefined);
      }
      if (status.relayOnly && !status.viaHelper) {
        maybeAutoSwitch(guestId ?? undefined);
      }
    } catch {
      if (token === generation) {
        useRouteStore.setState({
          relayOnly: false,
          viaHelper: false,
          helperOffers: 0,
          pathLabel: null,
          pathTrust: null,
          digestVerified: false,
          retryCount: 0,
          degraded: true,
        });
      }
    }
  };
  void pull();
  const timer = window.setInterval(() => void pull(), 2000);
  return () => {
    if (token === generation) {
      generation += 1;
    }
    window.clearInterval(timer);
  };
}

export async function refreshHelpers(exclude?: string) {
  if (!isTauri()) {
    return;
  }
  if (helperRefreshInFlight) {
    return helperRefreshInFlight;
  }
  useRouteStore.setState({ probing: true, probeError: null });
  helperRefreshInFlight = (async () => {
    try {
      const helpers = await helperList(exclude);
      useRouteStore.setState({ helpers, probing: false });
      lastHelperRefresh = Date.now();
    } catch (error) {
      useRouteStore.setState({
        probing: false,
        probeError: error instanceof Error ? error.message : String(error),
      });
    } finally {
      helperRefreshInFlight = null;
    }
  })();
  return helperRefreshInFlight;
}

export async function switchHelper(helperId: string) {
  if (!isTauri()) {
    return;
  }
  useRouteStore.setState({ switching: true, probeError: null });
  try {
    const stage = await useHelper(helperId);
    useSession.setState({ stage, lastSeenId: stage.endpointId ?? null, error: null });
    useRouteStore.setState({
      switching: false,
      viaHelper: true,
      relayOnly: false,
      pathTrust: null,
      digestVerified: false,
      retryCount: 0,
      degraded: true,
    });
  } catch (error) {
    useRouteStore.setState({
      switching: false,
      probeError: error instanceof Error ? error.message : String(error),
    });
  }
}

function maybeAutoSwitch(exclude?: string) {
  const now = Date.now();
  if (now - lastAuto < AUTO_INTERVAL_MS) {
    return;
  }
  const { helpers, switching, viaHelper, relayOnly } = useRouteStore.getState();
  if (!relayOnly || viaHelper || switching) {
    return;
  }
  const best = helpers.find((item) => item.usable);
  if (!best) {
    return;
  }
  lastAuto = now;
  void switchHelper(best.endpointId);
  void exclude;
}
