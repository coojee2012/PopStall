import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { usePlazaStore } from "../../stores/plaza";
import { useTalkStore } from "../../stores/talk";
import type { TalkLine } from "../../types/ipc";
import { Button } from "../ui/button";
import { cn } from "../../lib/utils";

const samples: TalkLine[] = [
  { id: "1", senderId: "preview-south", text: "夜里的爵士，有人在听吗？", mine: false },
  { id: "2", senderId: "preview-book", text: "在。雨声那首也可以。", mine: false },
  { id: "3", senderId: "me", text: "我把唱机对着窗。", mine: true },
];

const previewNames = new Map([
  ["preview-south", "南窗唱片"],
  ["preview-book", "慢船书店"],
]);

export function TalkRoom() {
  const desktop = isTauri();
  const live = useTalkStore((state) => state.lines);
  const problem = useTalkStore((state) => state.problem);
  const send = useTalkStore((state) => state.send);
  const stalls = usePlazaStore((state) => state.stalls);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const composing = useRef(false);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lines = desktop || !import.meta.env.DEV ? live : samples;
  const names = useMemo(() => new Map(stalls.map((stall) => [stall.endpointId, stall.title])), [stalls]);

  useEffect(() => {
    const node = scroller.current;
    if (!node || !stick.current) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [lines]);

  async function submit() {
    const text = draft.trim();
    if (!text || sending || !desktop) {
      return;
    }
    setSending(true);
    const ok = await send(text);
    setSending(false);
    if (ok) {
      setDraft("");
    }
  }

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="shrink-0 px-6 pt-5 pb-3">
        <h2 className="font-serif text-3xl font-medium tracking-tight">夜谈</h2>
        <p className="mt-1 max-w-xl text-sm leading-6 text-muted">旁边还在的人，会把他们留着的话递过来。最多 300 句，关掉软件就散。</p>
      </header>
      <div
        ref={scroller}
        onScroll={(event) => {
          const node = event.currentTarget;
          stick.current = node.scrollHeight - node.scrollTop - node.clientHeight < 80;
        }}
        className="plaza-scroll flex min-h-0 flex-1 flex-col overflow-y-auto px-6"
      >
        {lines.length === 0 ? (
          <div className="grid flex-1 place-items-center text-center">
            <div>
              <p className="font-serif text-2xl">还没有人说话</p>
              <p className="mt-2 text-sm leading-6 text-muted">说一句，在线的人就能接着看见。</p>
            </div>
          </div>
        ) : (
          <div className="mx-auto flex w-full max-w-2xl flex-1 flex-col justify-end gap-3 py-4">
            {lines.map((line, index) => {
              const previous = lines[index - 1];
              const showName = !previous || previous.senderId !== line.senderId || previous.mine !== line.mine;
              return (
                <article key={line.id} className={cn("flex", line.mine ? "justify-end" : "justify-start")}>
                  <div className={cn("max-w-[78%]", line.mine && "text-right")}>
                    {showName ? (
                      <p className="mb-1 px-1 text-xs text-muted">{line.mine ? "我" : (names.get(line.senderId) ?? previewNames.get(line.senderId) ?? shortId(line.senderId))}</p>
                    ) : null}
                    <p
                      className={cn(
                        "inline-block rounded-[1.25rem] px-3.5 py-2 text-left text-sm leading-6",
                        line.mine ? "rounded-br-md bg-ink text-paper" : "rounded-bl-md border border-line bg-card",
                      )}
                    >
                      {line.text}
                    </p>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>
      <form
        className="shrink-0 border-t border-line px-6 py-4"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <div className="mx-auto flex w-full max-w-2xl items-center gap-2">
          <input
            value={draft}
            maxLength={200}
            disabled={!desktop || sending}
            placeholder={desktop ? "说一句" : "在闪摊窗口里才能说话"}
            aria-label="说一句"
            onChange={(event) => setDraft(Array.from(event.target.value).slice(0, 200).join(""))}
            onCompositionStart={() => {
              composing.current = true;
            }}
            onCompositionEnd={() => {
              composing.current = false;
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !composing.current) {
                event.preventDefault();
                void submit();
              }
            }}
            className="min-w-0 flex-1 rounded-full border border-line bg-card px-4 py-2.5 text-sm outline-none focus:border-ember disabled:opacity-60"
          />
          <Button type="submit" disabled={!desktop || sending || draft.trim().length === 0}>
            {sending ? "送出" : "发送"}
          </Button>
        </div>
        {problem ? <p className="mx-auto mt-2 w-full max-w-2xl text-xs text-ember-dark">{problem}</p> : null}
      </form>
    </section>
  );
}

function shortId(id: string) {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 4)}…${id.slice(-4)}`;
}
