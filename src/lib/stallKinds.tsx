import {
  BookOpen,
  FileText,
  Film,
  Globe,
  Headphones,
  Image as ImageIcon,
  AppWindow,
  Store,
  type LucideIcon,
} from "lucide-react";
import type { PlazaStall } from "../types/ipc";

export type StallKind =
  | "music"
  | "image"
  | "video"
  | "novel"
  | "document"
  | "site"
  | "app"
  | "other";

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

type KindMeta = {
  label: string;
  Icon: LucideIcon;
  wash: string;
  chip: string;
  emptyBlurb: string;
};

const META: Record<StallKind, KindMeta> = {
  music: {
    label: "音乐",
    Icon: Headphones,
    wash: "bg-clay text-ember-dark",
    chip: "text-ember-dark bg-clay",
    emptyBlurb: "音频合集 · 点进去随时收听",
  },
  image: {
    label: "图片",
    Icon: ImageIcon,
    wash: "bg-moss text-pine",
    chip: "text-pine bg-moss",
    emptyBlurb: "图集 · 点开看大图",
  },
  video: {
    label: "视频",
    Icon: Film,
    wash: "bg-[#111827] text-white",
    chip: "text-ink bg-sidebar",
    emptyBlurb: "片单 · 大屏连播",
  },
  novel: {
    label: "小说",
    Icon: BookOpen,
    wash: "bg-sidebar text-ink",
    chip: "text-ink bg-sidebar",
    emptyBlurb: "书摊 · 本地直连阅读",
  },
  document: {
    label: "文档",
    Icon: FileText,
    wash: "bg-sidebar text-ink",
    chip: "text-ink bg-sidebar",
    emptyBlurb: "资料夹 · PDF / 文稿速览",
  },
  site: {
    label: "网站",
    Icon: Globe,
    wash: "bg-moss text-pine",
    chip: "text-pine bg-moss",
    emptyBlurb: "独立网站 · 实时访问",
  },
  app: {
    label: "应用",
    Icon: AppWindow,
    wash: "bg-clay text-ember-dark",
    chip: "text-ember-dark bg-clay",
    emptyBlurb: "本地应用 · 映射端口访问",
  },
  other: {
    label: "其他",
    Icon: Store,
    wash: "border border-line bg-card text-ink",
    chip: "text-muted bg-sidebar",
    emptyBlurb: "闪摊上的一间小店",
  },
};

export function stallKind(label: string): StallKind {
  if (label.startsWith("音乐")) return "music";
  if (label.startsWith("图片")) return "image";
  if (label.startsWith("视频")) return "video";
  if (label.startsWith("小说")) return "novel";
  if (label.startsWith("文档")) return "document";
  if (label.startsWith("原样") || label.includes("网站")) return "site";
  if (label.startsWith("本地应用")) return "app";
  return "other";
}

export function kindMeta(kind: StallKind): KindMeta {
  return META[kind];
}

export function kindName(kind: StallKind): string {
  return META[kind].label;
}

export function kindWash(kind: StallKind): string {
  return META[kind].wash;
}

export function kindChip(kind: StallKind): string {
  return META[kind].chip;
}

/** Prefer host blurb; otherwise a calm type-native subtitle. */
export function stallSubtitle(stall: Pick<PlazaStall, "blurb" | "label">): string {
  const trimmed = stall.blurb?.trim();
  if (trimmed) {
    return trimmed;
  }
  return META[stallKind(stall.label)].emptyBlurb;
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

/** @deprecated use kindMeta(kind).Icon */
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
