import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { usePlazaStore } from "../../stores/plaza";
import { useSession } from "../../stores/session";
import { useTalkStore } from "../../stores/talk";
import type { PlazaStall, TalkLine } from "../../types/ipc";
import { Button } from "../ui/button";
import { cn } from "../../lib/utils";

const NICK_KEY = "popstall.nick.v1";
const NAME_LIMIT = 12;

const samples: TalkLine[] = [
  { id: "1", senderId: "preview-south", name: "阿宁", text: "夜里的爵士，有人在听吗？", mine: false },
  { id: "2", senderId: "preview-book", name: "小林", text: "在。雨声那首也可以。", mine: false },
  { id: "3", senderId: "me", name: "南窗", text: "我把唱机对着窗。", mine: true },
];

const washes = ["bg-clay text-ember-dark", "bg-moss text-pine", "bg-[#efe6d6] text-ink", "bg-[#f3e4ea] text-[#7a4458]", "bg-[#e6eef2] text-[#3d5c6e]"];

export function TalkRoom() {
  const desktop = isTauri();
  const live = useTalkStore((state) => state.lines);
  const problem = useTalkStore((state) => state.problem);
  const send = useTalkStore((state) => state.send);
  const stalls = usePlazaStore((state) => state.stalls);
  const openDiscovered = useSession((state) => state.openDiscovered);
  const busy = useSession((state) => state.guestBusy);
  const [nick, setNick] = useState(loadNick);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [ask, setAsk] = useState<PlazaStall | null>(null);
  const [pass, setPass] = useState("");
  const [passError, setPassError] = useState<string | null>(null);
  const composing = useRef(false);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lines = desktop || !import.meta.env.DEV ? live : samples;
  const stallById = useMemo(() => new Map(stalls.map((stall) => [stall.endpointId, stall])), [stalls]);
  const spokenNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const line of lines) {
      if (line.name) {
        names.set(line.senderId, line.name);
      }
    }
    return names;
  }, [lines]);

  useEffect(() => {
    const node = scroller.current;
    if (!node || !stick.current) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [lines]);

  function rememberNick(value: string) {
    const next = clipName(value);
    setNick(next);
    try {
      localStorage.setItem(NICK_KEY, next);
    } catch {
      // The name still goes out with the next sentence.
    }
    return next;
  }

  async function submit() {
    const text = draft.trim();
    if (!text || sending || !desktop) {
      return;
    }
    setSending(true);
    const ok = await send(text, nick);
    setSending(false);
    if (ok) {
      setDraft("");
    }
  }

  function enterStall(stall: PlazaStall) {
    if (stall.locked) {
      setAsk(stall);
      setPass("");
      setPassError(null);
      return;
    }
    void openDiscovered(stall.endpointId, stall.title, { label: stall.label, blurb: stall.blurb });
  }

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="flex shrink-0 items-start justify-between gap-4 px-6 pt-5 pb-3">
        <div>
          <h2 className="font-serif text-3xl font-medium tracking-tight">夜谈</h2>
          <p className="mt-1 max-w-xl text-sm leading-6 text-muted">用你自己的名字说话。对方还开着摊的话，名字底下有一扇门。</p>
        </div>
        <NameSeat nick={nick} onSave={rememberNick} />
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
              <p className="font-serif text-2xl">{nick ? "还没有人说话" : "先起个名字"}</p>
              <p className="mt-2 text-sm leading-6 text-muted">{nick ? "说一句，在线的人就能接着看见。" : "名字会跟着你的话走。摊名不再当作称呼。"}</p>
            </div>
          </div>
        ) : (
          <div className="mx-auto flex w-full max-w-2xl flex-1 flex-col justify-end gap-3 py-4">
            {lines.map((line, index) => {
              const previous = lines[index - 1];
              const showName = !previous || previous.senderId !== line.senderId || previous.mine !== line.mine;
              const spoken = line.mine ? nick || spokenNames.get(line.senderId) || "" : spokenNames.get(line.senderId) || line.name;
              const label = spoken || (line.mine ? "我" : shortId(line.senderId));
              const stall = line.mine ? undefined : (stallById.get(line.senderId) ?? previewStall(line.senderId));
              return (
                <article key={line.id} className={cn("flex gap-2", line.mine ? "justify-end" : "justify-start")}>
                  {line.mine ? null : showName ? <Face name={label} seed={line.senderId} /> : <span className="size-8 shrink-0" />}
                  <div className={cn("max-w-[78%]", line.mine && "text-right")}>
                    {showName ? (
                      <p className={cn("mb-1 flex items-center gap-2 px-1 text-xs text-muted", line.mine && "justify-end")}>
                        <span>{label}</span>
                        {stall ? (
                          <button type="button" className="text-pine hover:underline" onClick={() => enterStall(stall)}>
                            在摆 {stall.title}
                          </button>
                        ) : null}
                      </p>
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
                  {line.mine ? (showName ? <Face name={label} seed="me" /> : <span className="size-8 shrink-0" />) : null}
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
            placeholder={desktop ? (nick ? `以${nick}的身份说一句` : "说一句") : "在闪摊窗口里才能说话"}
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
      {ask ? (
        <div className="fixed inset-0 z-30 flex items-start justify-center bg-ink/30 px-4 pt-[14vh]" onMouseDown={() => setAsk(null)}>
          <form
            className="w-full max-w-md rounded-3xl border border-line bg-card px-6 py-6 shadow-[0_24px_60px_rgba(36,25,16,0.16)]"
            onMouseDown={(event) => event.stopPropagation()}
            onSubmit={(event) => {
              event.preventDefault();
              const stall = ask;
              const password = pass.trim();
              if (!password) {
                setPassError("先写密码");
                return;
              }
              void openDiscovered(stall.endpointId, stall.title, {
                label: stall.label,
                blurb: stall.blurb,
                password,
              }).then((opened) => {
                if (opened) {
                  setAsk(null);
                  return;
                }
                setPassError(useSession.getState().error ?? "密码不对");
              });
            }}
          >
            <h2 className="font-serif text-2xl">进入{ask.title}</h2>
            <p className="mt-2 text-sm leading-6 text-muted">这间摊要密码。名字还是刚才说话的那个人。</p>
            <input
              type="password"
              value={pass}
              autoFocus
              autoComplete="off"
              maxLength={48}
              onChange={(event) => setPass(event.target.value)}
              className="mt-4 w-full rounded-full border border-line bg-paper px-4 py-2 text-sm outline-none focus:border-ember"
            />
            {passError ? <p className="mt-3 text-sm text-ember-dark">{passError}</p> : null}
            <div className="mt-4 flex gap-3">
              <Button type="submit" disabled={busy || pass.trim().length === 0}>
                {busy ? "正在确认" : "进去"}
              </Button>
              <Button variant="quiet" type="button" onClick={() => setAsk(null)}>
                取消
              </Button>
            </div>
          </form>
        </div>
      ) : null}
    </section>
  );
}

function NameSeat({ nick, onSave }: { nick: string; onSave: (value: string) => string }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(nick);
  const composing = useRef(false);

  useEffect(() => {
    if (!editing) {
      setDraft(nick);
    }
  }, [editing, nick]);

  function commit() {
    onSave(draft);
    setEditing(false);
  }

  if (editing) {
    return (
      <form
        className="flex shrink-0 items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          commit();
        }}
      >
        <input
          value={draft}
          autoFocus
          maxLength={NAME_LIMIT}
          aria-label="你的名字"
          placeholder="怎么称呼你"
          onChange={(event) => setDraft(Array.from(event.target.value).slice(0, NAME_LIMIT).join(""))}
          onCompositionStart={() => {
            composing.current = true;
          }}
          onCompositionEnd={() => {
            composing.current = false;
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              setDraft(nick);
              setEditing(false);
            }
            if (event.key === "Enter" && !composing.current) {
              event.preventDefault();
              commit();
            }
          }}
          className="w-36 rounded-full border border-line bg-card px-3 py-1.5 text-sm outline-none focus:border-ember"
        />
        <Button type="submit" className="px-3 py-1.5">
          用这个
        </Button>
      </form>
    );
  }

  return (
    <button
      type="button"
      onClick={() => setEditing(true)}
      className="flex shrink-0 items-center gap-2 rounded-full border border-line bg-card py-1 pr-3 pl-1 text-left disabled:opacity-60"
    >
      <Face name={nick || "名"} seed={nick || "seat"} />
      <span>
        <span className="block text-sm">{nick || "起个名字"}</span>
        <span className="block text-[11px] text-muted">{nick ? "改名字" : "最多 12 个字"}</span>
      </span>
    </button>
  );
}

function Face({ name, seed }: { name: string; seed: string }) {
  const letter = Array.from(name)[0] || "谈";
  let hash = 0;
  for (const char of seed) {
    hash = (hash + char.charCodeAt(0)) % washes.length;
  }
  return <span className={cn("grid size-8 shrink-0 place-items-center rounded-full font-serif text-sm", washes[hash])}>{letter}</span>;
}

function previewStall(senderId: string): PlazaStall | undefined {
  if (isTauri() || !import.meta.env.DEV || senderId !== "preview-south") {
    return undefined;
  }
  return { endpointId: senderId, title: "南窗唱片", label: "音乐展厅", blurb: "", locked: false };
}

function loadNick() {
  try {
    return clipName(localStorage.getItem(NICK_KEY) ?? "");
  } catch {
    return "";
  }
}

function clipName(value: string) {
  return Array.from(value.trim()).slice(0, NAME_LIMIT).join("");
}

function shortId(id: string) {
  if (id.length <= 10) {
    return id;
  }
  return `${id.slice(0, 4)}…${id.slice(-4)}`;
}
