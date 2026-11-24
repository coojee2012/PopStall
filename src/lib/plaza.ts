import type { PlazaStall } from "../types/ipc";

export type StallKind = "music" | "image" | "video" | "novel" | "document" | "site" | "app" | "other";

export type KindFilter = StallKind | "all";

export const kindFilters: { id: KindFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "music", label: "音乐" },
  { id: "image", label: "图片" },
  { id: "video", label: "视频" },
  { id: "novel", label: "小说" },
  { id: "document", label: "文档" },
  { id: "site", label: "网站" },
  { id: "app", label: "应用" },
];

export function stallKind(label: string): StallKind {
  if (label.startsWith("音乐")) {
    return "music";
  }
  if (label.startsWith("图片")) {
    return "image";
  }
  if (label.startsWith("视频")) {
    return "video";
  }
  if (label.startsWith("小说")) {
    return "novel";
  }
  if (label.startsWith("文档")) {
    return "document";
  }
  if (label.startsWith("原样") || label.includes("网站")) {
    return "site";
  }
  if (label.startsWith("本地应用")) {
    return "app";
  }
  return "other";
}

export function kindGlyph(kind: StallKind): string {
  switch (kind) {
    case "music":
      return "乐";
    case "image":
      return "图";
    case "video":
      return "影";
    case "novel":
      return "文";
    case "document":
      return "档";
    case "site":
      return "站";
    case "app":
      return "用";
    default:
      return "摊";
  }
}

export function kindName(kind: StallKind): string {
  return kindFilters.find((item) => item.id === kind)?.label ?? "其他";
}

export function kindWash(kind: StallKind): string {
  switch (kind) {
    case "music":
    case "app":
      return "bg-clay text-ember-dark";
    case "image":
    case "site":
      return "bg-moss text-pine";
    default:
      return "border border-line bg-card text-ink";
  }
}

export function matchesQuery(stall: Pick<PlazaStall, "title" | "label" | "blurb">, terms: string[]) {
  if (terms.length === 0) {
    return true;
  }
  const haystack = `${stall.title}\n${stall.label}\n${stall.blurb}`.toLowerCase();
  return terms.every((term) => haystack.includes(term));
}

export function sameStalls(left: PlazaStall[], right: PlazaStall[]) {
  if (left.length !== right.length) {
    return false;
  }
  for (let index = 0; index < left.length; index += 1) {
    const a = left[index];
    const b = right[index];
    if (
      a.endpointId !== b.endpointId ||
      a.title !== b.title ||
      a.label !== b.label ||
      a.blurb !== b.blurb ||
      a.locked !== b.locked
    ) {
      return false;
    }
  }
  return true;
}
