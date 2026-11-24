import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { HostInfo, PlazaSnapshot, RouteStatus, ScanReport, StageInfo, TrafficSnapshot } from "../types/ipc";

export function inDesktopApp(): boolean {
  return isTauri();
}

function assertDesktop(): void {
  if (!isTauri()) {
    throw new Error("请使用「闪摊」桌面窗口。浏览器页面没有系统权限，不能选择文件夹或建立连接。");
  }
}

export async function pickFolder(): Promise<string | null> {
  assertDesktop();
  const selected = await open({
    directory: true,
    multiple: false,
    title: "选择要开张的文件夹",
  });
  return typeof selected === "string" ? selected : null;
}

export function scanFolder(root: string): Promise<ScanReport> {
  assertDesktop();
  return invoke("scan_folder", { root });
}

export function startHost(input: {
  mode: string;
  root?: string;
  port?: number;
  blurb?: string;
  password?: string;
}): Promise<HostInfo> {
  assertDesktop();
  const payload: { mode: string; root?: string; port?: number; blurb?: string; password?: string } = {
    mode: input.mode,
  };
  if (input.root) {
    payload.root = input.root;
  }
  if (input.port) {
    payload.port = input.port;
  }
  if (input.blurb) {
    payload.blurb = input.blurb;
  }
  if (input.password) {
    payload.password = input.password;
  }
  return invoke("start_host", payload);
}

export function stopHost(): Promise<void> {
  assertDesktop();
  return invoke("stop_host");
}

export function hostStatus(): Promise<HostInfo | null> {
  assertDesktop();
  return invoke("host_status");
}

export function listPlaza(): Promise<PlazaSnapshot> {
  assertDesktop();
  return invoke("list_plaza");
}

export function traffic(): Promise<TrafficSnapshot> {
  assertDesktop();
  return invoke("traffic");
}

export function routeStatus(): Promise<RouteStatus> {
  assertDesktop();
  return invoke("route_status");
}

export function openDiscovered(endpointId: string, title: string, password?: string): Promise<StageInfo> {
  assertDesktop();
  return invoke("open_discovered", { endpointId, title, password: password ?? "" });
}

export function openGuest(shareCode: string, password?: string): Promise<StageInfo> {
  assertDesktop();
  return invoke("open_guest", { shareCode, password: password ?? "" });
}

export function openPreview(url: string, title: string): Promise<StageInfo> {
  assertDesktop();
  return invoke("open_preview", { url, title });
}

export function placeStage(bounds: { x: number; y: number; width: number; height: number }): Promise<void> {
  assertDesktop();
  return invoke("place_stage_bounds", bounds);
}

export function closeStage(): Promise<void> {
  assertDesktop();
  return invoke("close_stage");
}
