import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import { routeStatus } from "../lib/api";

type RouteState = {
  relayOnly: boolean;
  viaHelper: boolean;
  helperOffers: number;
};

export const useRouteStore = create<RouteState>(() => ({
  relayOnly: false,
  viaHelper: false,
  helperOffers: 0,
}));

let generation = 0;

export function startRoute(): () => void {
  const token = ++generation;
  if (!isTauri()) {
    useRouteStore.setState({ relayOnly: false, viaHelper: false, helperOffers: 0 });
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
      });
    } catch {
      if (token === generation) {
        useRouteStore.setState({ relayOnly: false, viaHelper: false, helperOffers: 0 });
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
