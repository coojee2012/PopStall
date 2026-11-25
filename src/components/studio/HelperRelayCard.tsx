import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import * as api from "../../lib/api";
import { errorText } from "../../lib/utils";
import { Button } from "../ui/button";
import { Card } from "../ui/card";

const PREF_KEY = "popstall.helper.v1";
export const DEFAULT_LIMIT_BPS = 1024 * 1024;

type Pref = {
  enabled: boolean;
  limitBps: number;
  listenPort: string;
};

const limits = [
  { label: "512 KB/s", value: 512 * 1024 },
  { label: "1 MB/s", value: 1024 * 1024 },
  { label: "2 MB/s", value: 2 * 1024 * 1024 },
  { label: "4 MB/s", value: 4 * 1024 * 1024 },
];

function loadPref(): Pref {
  try {
    const raw = localStorage.getItem(PREF_KEY);
    if (!raw) {
      return { enabled: false, limitBps: DEFAULT_LIMIT_BPS, listenPort: "" };
    }
    const parsed = JSON.parse(raw) as Partial<Pref>;
    return {
      enabled: Boolean(parsed.enabled),
      limitBps: typeof parsed.limitBps === "number" ? parsed.limitBps : DEFAULT_LIMIT_BPS,
      listenPort: typeof parsed.listenPort === "string" ? parsed.listenPort : "",
    };
  } catch {
    return { enabled: false, limitBps: DEFAULT_LIMIT_BPS, listenPort: "" };
  }
}

function savePref(pref: Pref) {
  try {
    localStorage.setItem(PREF_KEY, JSON.stringify(pref));
  } catch {
    // Preference still applies for this session.
  }
}

function parseListenPort(raw: string): number | null {
  const trimmed = raw.trim();
  if (!trimmed) {
    return null;
  }
  const value = Number(trimmed);
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new Error("端口请填 1–65535 的整数，或留空用随机端口");
  }
  return value;
}

export function HelperRelayCard() {
  const desktop = isTauri();
  const [enabled, setEnabled] = useState(false);
  const [limitBps, setLimitBps] = useState(DEFAULT_LIMIT_BPS);
  const [listenPort, setListenPort] = useState("");
  const [portNote, setPortNote] = useState("");
  const [boundPorts, setBoundPorts] = useState<number[]>([]);
  const [active, setActive] = useState(0);
  const [capable, setCapable] = useState(false);
  const [reachNote, setReachNote] = useState("正在看这台机器能不能被直连…");
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  function syncStatus(status: Awaited<ReturnType<typeof api.helperStatus>>) {
    setEnabled(status.enabled);
    setLimitBps(status.limitBps);
    setActive(status.active);
    setCapable(status.capable);
    setReachNote(status.reachNote);
    setPortNote(status.portNote ?? "");
    setBoundPorts(status.boundPorts ?? []);
    if (status.listenPort != null && status.listenPort > 0) {
      setListenPort(String(status.listenPort));
    }
  }

  useEffect(() => {
    const pref = loadPref();
    setLimitBps(pref.limitBps);
    setListenPort(pref.listenPort);
    if (!desktop) {
      setEnabled(false);
      setCapable(true);
      setReachNote("浏览器预览不检测网络。真正当中继要在闪摊窗口里。");
      return;
    }
    void (async () => {
      try {
        let port: number | null = null;
        try {
          port = parseListenPort(pref.listenPort);
        } catch (error) {
          setProblem(errorText(error));
          port = null;
        }
        const status = await api.helperSet(pref.enabled, pref.limitBps, port);
        syncStatus(status);
        savePref({
          enabled: status.enabled,
          limitBps: status.limitBps,
          listenPort: status.listenPort ? String(status.listenPort) : pref.listenPort,
        });
        if (pref.enabled && !status.enabled) {
          savePref({
            enabled: false,
            limitBps: status.limitBps,
            listenPort: status.listenPort ? String(status.listenPort) : pref.listenPort,
          });
        }
      } catch (error) {
        setEnabled(false);
        savePref({ enabled: false, limitBps: pref.limitBps, listenPort: pref.listenPort });
        setProblem(errorText(error));
        try {
          const status = await api.helperStatus();
          syncStatus(status);
        } catch {
          // Keep the error from helperSet.
        }
      }
    })();
  }, [desktop]);

  useEffect(() => {
    if (!desktop) {
      return undefined;
    }
    const timer = window.setInterval(() => {
      void (async () => {
        try {
          const status = await api.helperStatus();
          syncStatus(status);
          if (!status.enabled) {
            const pref = loadPref();
            if (pref.enabled) {
              savePref({
                enabled: false,
                limitBps: pref.limitBps,
                listenPort: pref.listenPort,
              });
            }
          }
        } catch {
          // Keep the last known note.
        }
      })();
    }, 4000);
    return () => window.clearInterval(timer);
  }, [desktop]);

  async function apply(nextEnabled: boolean, nextLimit: number, nextPortRaw: string) {
    setBusy(true);
    setProblem(null);
    let port: number | null;
    try {
      port = parseListenPort(nextPortRaw);
    } catch (error) {
      setProblem(errorText(error));
      setBusy(false);
      return;
    }
    if (!desktop) {
      setEnabled(nextEnabled);
      setLimitBps(nextLimit);
      setListenPort(nextPortRaw.trim());
      setBusy(false);
      setAsk(false);
      return;
    }
    try {
      const status = await api.helperSet(nextEnabled, nextLimit, port);
      syncStatus(status);
      savePref({
        enabled: status.enabled,
        limitBps: status.limitBps,
        listenPort: status.listenPort ? String(status.listenPort) : nextPortRaw.trim(),
      });
      setAsk(false);
    } catch (error) {
      setProblem(errorText(error));
      savePref({
        enabled: false,
        limitBps: nextLimit,
        listenPort: nextPortRaw.trim(),
      });
      try {
        const status = await api.helperStatus();
        syncStatus(status);
      } catch {
        setEnabled(false);
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Card className="flex flex-col gap-4">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h3 className="font-serif text-2xl font-medium tracking-tight">我来当中继</h3>
            <p className="mt-2 max-w-xl text-sm leading-6 text-muted">
              打开后，别人卡在慢公共中继时，可以把密文流量经你转发。默认限速 1 MB/s。若路由器做了 UDP 端口转发，在下方填同一端口；改端口后需重启闪摊才会真正监听。
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            disabled={busy || (!enabled && !capable)}
            onClick={() => {
              if (enabled) {
                void apply(false, limitBps, listenPort);
                return;
              }
              setAsk(true);
            }}
            className={`relative h-8 w-14 shrink-0 rounded-full transition disabled:opacity-40 ${enabled ? "bg-pine" : "bg-line"}`}
          >
            <span
              className={`absolute top-1 size-6 rounded-full bg-card shadow transition ${enabled ? "left-7" : "left-1"}`}
            />
          </button>
        </div>

        <label className="flex flex-col gap-1.5">
          <span className="text-xs text-muted">固定 UDP 端口（可选，例如 35175）</span>
          <div className="flex flex-wrap items-center gap-2">
            <input
              type="text"
              inputMode="numeric"
              placeholder="留空则随机"
              value={listenPort}
              disabled={busy || enabled}
              onChange={(event) => setListenPort(event.target.value)}
              className="w-36 rounded-[0.9rem] border border-line bg-paper px-3 py-2 font-mono text-sm outline-none focus:border-ink disabled:opacity-50"
            />
            <Button
              type="button"
              className="px-3 py-1.5 text-xs"
              disabled={busy || enabled}
              onClick={() => void apply(false, limitBps, listenPort)}
            >
              仅保存端口
            </Button>
          </div>
          {portNote ? <p className="text-xs leading-5 text-muted">{portNote}</p> : null}
          {boundPorts.length > 0 ? (
            <p className="font-mono text-[11px] text-muted">当前监听：{boundPorts.join(" / ")}</p>
          ) : null}
        </label>

        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs text-muted">限速</span>
          {limits.map((item) => (
            <button
              key={item.value}
              type="button"
              disabled={busy}
              onClick={() => void apply(enabled, item.value, listenPort)}
              className={`rounded-full border px-3 py-1 text-xs transition ${
                limitBps === item.value ? "border-ink bg-ink text-paper" : "border-line bg-card text-ink hover:border-[#d7b79a]"
              }`}
            >
              {item.label}
            </button>
          ))}
        </div>

        {enabled ? (
          <p className="text-xs leading-5 text-pine">
            正在当互助中继 · 限速 {(limitBps / (1024 * 1024)).toFixed(limitBps % (1024 * 1024) === 0 ? 0 : 1)} MB/s
            {active > 0 ? ` · 转发中 ${active} 路` : ""}
          </p>
        ) : (
          <p className={`text-xs leading-5 ${capable ? "text-pine" : "text-muted"}`}>{reachNote}</p>
        )}
        {problem ? <p className="text-xs text-ember-dark">{problem}</p> : null}
        {!desktop ? <p className="text-xs text-muted">浏览器里只是预览。真正转发要在闪摊窗口里打开。</p> : null}
      </Card>

      {ask ? (
        <div className="fixed inset-0 z-40 grid place-items-center bg-ink/40 px-4" role="dialog" aria-modal="true">
          <div className="w-full max-w-md rounded-[1.4rem] border border-line bg-card p-5 shadow-lg">
            <h4 className="font-serif text-2xl">确认当互助中继？</h4>
            <p className="mt-3 text-sm leading-6 text-muted">
              别人的流量会占用你的上行。内容仍是加密的，你看不到文件。默认限速 1 MB/s，可随时关掉。
              {listenPort.trim()
                ? ` 将使用固定端口 ${listenPort.trim()}；若与当前监听不一致，需先重启闪摊。`
                : " 未填固定端口时使用随机端口。"}
            </p>
            <p className="mt-2 text-sm leading-6 text-pine">{reachNote}</p>
            <div className="mt-5 flex justify-end gap-2">
              <Button type="button" className="bg-transparent text-ink hover:bg-paper" onClick={() => setAsk(false)}>
                先不要
              </Button>
              <Button type="button" disabled={busy || !capable} onClick={() => void apply(true, limitBps, listenPort)}>
                {busy ? "正在检测" : "打开中继"}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}
