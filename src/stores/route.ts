import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import { helperList, routeStatus, useHelper } from "../lib/api";
import type { HelperOffer } from "../types/ipc";
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
}));

const AUTO_INTERVAL_MS = 45_000;
let generation = 0;
let lastAuto = 0;

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
      });
      const stage = useSession.getState().stage;
      const guestId = stage?.kind === "guest" ? stage.endpointId : null;
      if (status.relayOnly && !status.viaHelper && (status.helperOffers > 0 || guestId)) {
        await refreshHelpers(guestId ?? undefined);
        maybeAutoSwitch(guestId ?? undefined);
      }
    } catch {
      if (token === generation) {
        useRouteStore.setState({
          relayOnly: false,
          viaHelper: false,
          helperOffers: 0,
          pathLabel: null,
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
  useRouteStore.setState({ probing: true, probeError: null });
  try {
    const helpers = await helperList(exclude);
    useRouteStore.setState({ helpers, probing: false });
  } catch (error) {
    useRouteStore.setState({
      probing: false,
      probeError: error instanceof Error ? error.message : String(error),
    });
  }
}

export async function switchHelper(helperId: string) {
  if (!isTauri()) {
    return;
  }
  useRouteStore.setState({ switching: true, probeError: null });
  try {
    const stage = await useHelper(helperId);
    useSession.setState({ stage, lastSeenId: stage.endpointId ?? null, error: null });
    useRouteStore.setState({ switching: false, viaHelper: true, relayOnly: false });
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
