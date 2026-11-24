import { isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";
import { routeStatus } from "../lib/api";

type RouteState = {
  relayOnly: boolean;
};

export const useRouteStore = create<RouteState>(() => ({
  relayOnly: false,
}));

let generation = 0;

export function startRoute(): () => void {
  const token = ++generation;
  if (!isTauri()) {
    useRouteStore.setState({ relayOnly: false });
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
      useRouteStore.setState({ relayOnly: status.relayOnly });
    } catch {
      if (token === generation) {
        useRouteStore.setState({ relayOnly: false });
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
