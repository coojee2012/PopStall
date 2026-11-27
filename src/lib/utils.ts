import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { PathTrustValue } from "../types/ipc";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function errorText(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "操作没有完成";
}

export type RoutePathKind = "direct" | "relay" | "helper" | "unknown";

export type RouteStatusNote = {
  kind: RoutePathKind;
  label: string;
  warnings: string[];
};

type RouteStatusInput = {
  pathTrust: PathTrustValue;
  pathLabel?: string | null;
  viaHelper: boolean;
  relayOnly: boolean;
  digestVerified: boolean;
  retryCount: number;
  degraded: boolean;
};

function pathKind(value: PathTrustValue): RoutePathKind {
  const kind = typeof value === "string" ? value : value?.kind;
  if (!kind) {
    return "unknown";
  }
  const text = kind.toLowerCase();
  if (text.includes("helper")) {
    return "helper";
  }
  if (text.includes("relay")) {
    return "relay";
  }
  if (text.includes("direct") || text.includes("endtoend") || text.includes("e2e")) {
    return "direct";
  }
  return "unknown";
}

export function routeStatusNote(input: RouteStatusInput): RouteStatusNote {
  const declared = pathKind(input.pathTrust);
  const kind =
    input.viaHelper || declared === "helper"
      ? "helper"
      : input.relayOnly || declared === "relay"
        ? "relay"
        : declared === "direct"
          ? "direct"
          : "unknown";
  const label =
    kind === "helper"
      ? "互助转发（不可信但可用）"
      : kind === "relay"
        ? "公共中继"
        : kind === "direct"
          ? "直连"
          : input.pathLabel?.trim() || "路径待确认";
  const warnings: string[] = [];
  if (input.digestVerified === false) {
    warnings.push("摘要未确认");
  }
  if (input.degraded) {
    warnings.push("网络降级");
  }
  if (input.retryCount > 0) {
    warnings.push(`已重试 ${input.retryCount} 次`);
  }
  return { kind, label, warnings };
}
