/**
 * Multi-mode stage workbench skeletons (React + Tailwind).
 * Content currently renders inside the Tauri child webview (exhibit.html);
 * these shells define the target interaction layout for each StallKind so
 * React chrome / future migration stay aligned.
 */
import {
  BookOpen,
  ChevronLeft,
  ChevronRight,
  FileText,
  Film,
  Globe,
  Headphones,
  Maximize2,
  Pause,
  Play,
  RefreshCw,
  SkipBack,
  SkipForward,
  Volume2,
} from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "../../../lib/utils";

export function NovelWorkbench({
  chapters = ["第一章", "第二章", "第三章"],
  active = 0,
}: {
  chapters?: string[];
  active?: number;
}) {
  return (
    <div className="flex h-full min-h-0 bg-paper">
      <aside className="flex w-64 shrink-0 flex-col border-r border-line bg-sidebar">
        <div className="border-b border-line px-3 py-2">
          <input
            type="search"
            placeholder="找章节"
            className="w-full rounded-full border border-line bg-card px-3 py-1.5 text-[12px] outline-none focus:border-line-strong"
          />
        </div>
        <ul className="plaza-scroll min-h-0 flex-1 overflow-y-auto p-2">
          {chapters.map((title, index) => (
            <li key={title}>
              <button
                type="button"
                className={cn(
                  "flex w-full items-center gap-2 rounded-xl px-2.5 py-2 text-left text-[13px]",
                  index === active ? "bg-card font-medium text-ink ring-1 ring-line" : "text-muted hover:bg-card/70",
                )}
              >
                <BookOpen className="size-3.5 shrink-0 text-soft" />
                <span className="truncate">{title}</span>
              </button>
            </li>
          ))}
        </ul>
      </aside>
      <div className="plaza-scroll min-h-0 flex-1 overflow-y-auto">
        <article className="mx-auto max-w-[72ch] px-8 py-10 font-serif text-[18px] leading-[1.85] text-ink">
          <p className="text-muted">选左侧章节开始阅读。行宽约 65–72ch，字号与护眼纸色由展厅顶栏调节。</p>
        </article>
      </div>
    </div>
  );
}

export function MusicWorkbench({
  tracks = ["曲目一", "曲目二", "曲目三"],
  current = 0,
}: {
  tracks?: string[];
  current?: number;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col bg-paper">
      <div className="plaza-scroll min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <ul className="mx-auto flex max-w-3xl flex-col gap-0.5">
          {tracks.map((title, index) => (
            <li key={title}>
              <button
                type="button"
                className={cn(
                  "flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-[13px]",
                  index === current
                    ? "bg-clay text-ember-dark"
                    : "text-ink hover:bg-sidebar",
                )}
              >
                <Headphones className="size-3.5 shrink-0 text-soft" />
                <span className="min-w-0 flex-1 truncate font-medium">{title}</span>
                <span className="text-[11px] text-soft tabular-nums">3:24</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
      <div className="shrink-0 border-t border-line bg-card px-4 py-3">
        <div className="mx-auto flex max-w-3xl items-center gap-3">
          <div className="min-w-0 flex-1">
            <p className="truncate text-[13px] font-semibold">{tracks[current]}</p>
            <div className="mt-2 h-1 overflow-hidden rounded-full bg-sidebar">
              <div className="h-full w-1/3 rounded-full bg-ember" />
            </div>
          </div>
          <div className="flex items-center gap-1">
            <IconBtn label="上一首">
              <SkipBack className="size-4" />
            </IconBtn>
            <IconBtn label="播放" strong>
              <Play className="size-4" />
            </IconBtn>
            <IconBtn label="下一首">
              <SkipForward className="size-4" />
            </IconBtn>
            <IconBtn label="音量">
              <Volume2 className="size-4" />
            </IconBtn>
          </div>
        </div>
      </div>
    </div>
  );
}

export function VideoWorkbench({
  episodes = ["第 1 集", "第 2 集", "第 3 集"],
  current = 0,
}: {
  episodes?: string[];
  current?: number;
}) {
  return (
    <div className="flex h-full min-h-0 bg-[#0c0a08] text-[#f6efe4]">
      <aside className="flex w-56 shrink-0 flex-col border-r border-white/10 bg-black/40">
        <p className="px-3 py-2 text-[11px] tracking-wide text-white/50">片单</p>
        <ul className="plaza-scroll min-h-0 flex-1 overflow-y-auto p-2">
          {episodes.map((title, index) => (
            <li key={title}>
              <button
                type="button"
                className={cn(
                  "flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left text-[12px]",
                  index === current ? "bg-white/10 text-white" : "text-white/60 hover:bg-white/5",
                )}
              >
                <Film className="size-3.5 shrink-0" />
                <span className="truncate">{title}</span>
              </button>
            </li>
          ))}
        </ul>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="relative grid min-h-0 flex-1 place-items-center bg-black">
          <Film className="size-12 text-white/20" />
        </div>
        <div className="flex items-center gap-2 border-t border-white/10 px-4 py-2.5">
          <IconBtn label="播放" strong>
            <Pause className="size-4" />
          </IconBtn>
          <div className="h-1 min-w-0 flex-1 rounded-full bg-white/15">
            <div className="h-full w-2/5 rounded-full bg-ember" />
          </div>
          <span className="text-[11px] text-white/50 tabular-nums">1.0×</span>
          <IconBtn label="全屏">
            <Maximize2 className="size-4" />
          </IconBtn>
        </div>
      </div>
    </div>
  );
}

export function DocumentWorkbench({
  files = ["README.md", "notes.pdf", "draft.txt"],
  active = 0,
}: {
  files?: string[];
  active?: number;
}) {
  return (
    <div className="flex h-full min-h-0 bg-paper">
      <aside className="w-60 shrink-0 overflow-y-auto border-r border-line bg-sidebar p-2">
        {files.map((name, index) => (
          <button
            key={name}
            type="button"
            className={cn(
              "mb-0.5 flex w-full items-center gap-2 rounded-xl px-2.5 py-2 text-left text-[13px]",
              index === active ? "bg-card font-medium ring-1 ring-line" : "text-muted hover:bg-card/70",
            )}
          >
            <FileText className="size-3.5 text-soft" />
            <span className="truncate">{name}</span>
          </button>
        ))}
      </aside>
      <div className="plaza-scroll min-h-0 flex-1 overflow-y-auto px-8 py-8">
        <div className="mx-auto max-w-[72ch] rounded-2xl border border-line bg-card px-6 py-5 text-[14px] leading-7 text-ink shadow-[0_1px_2px_rgba(28,25,23,0.04)]">
          Markdown / PDF / 文本在此干净排版预览，避免原始代码墙与厚边框。
        </div>
      </div>
    </div>
  );
}

export function SiteWorkbench({ path = "/" }: { path?: string }) {
  return (
    <div className="flex h-full min-h-0 flex-col bg-paper">
      <div className="flex shrink-0 items-center gap-2 border-b border-line bg-card px-3 py-2">
        <IconBtn label="后退">
          <ChevronLeft className="size-4" />
        </IconBtn>
        <IconBtn label="前进">
          <ChevronRight className="size-4" />
        </IconBtn>
        <IconBtn label="刷新">
          <RefreshCw className="size-3.5" />
        </IconBtn>
        <div className="flex min-w-0 flex-1 items-center gap-2 rounded-full border border-line bg-sidebar px-3 py-1.5 text-[12px] text-muted">
          <Globe className="size-3.5 shrink-0" />
          <span className="truncate">{path}</span>
        </div>
        <span className="hidden text-[11px] text-soft sm:inline">沙箱隔离 · 可在外部浏览器打开</span>
      </div>
      <div className="grid min-h-0 flex-1 place-items-center bg-sidebar text-[13px] text-muted">
        WebView 在此渲染完整网页
      </div>
    </div>
  );
}

function IconBtn({
  label,
  strong,
  children,
}: {
  label: string;
  strong?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      className={cn(
        "grid size-8 place-items-center rounded-full transition",
        strong
          ? "bg-ember text-white"
          : "text-current/70 hover:bg-black/5 hover:text-current",
      )}
    >
      {children}
    </button>
  );
}
