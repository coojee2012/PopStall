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
      return { enabled: false, limitBps: DEFAULT_LIMIT_BPS };
    }
    const parsed = JSON.parse(raw) as Partial<Pref>;
    return {
      enabled: Boolean(parsed.enabled),
      limitBps: typeof parsed.limitBps === "number" ? parsed.limitBps : DEFAULT_LIMIT_BPS,
    };
  } catch {
    return { enabled: false, limitBps: DEFAULT_LIMIT_BPS };
  }
}

function savePref(pref: Pref) {
  try {
    localStorage.setItem(PREF_KEY, JSON.stringify(pref));
  } catch {
    // Preference still applies for this session.
  }
}

export function HelperRelayCard() {
  const desktop = isTauri();
  const [enabled, setEnabled] = useState(false);
  const [limitBps, setLimitBps] = useState(DEFAULT_LIMIT_BPS);
  const [active, setActive] = useState(0);
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    const pref = loadPref();
    setLimitBps(pref.limitBps);
    if (!desktop) {
      setEnabled(pref.enabled);
      return;
    }
    void (async () => {
      try {
        const status = await api.helperSet(pref.enabled, pref.limitBps);
        setEnabled(status.enabled);
        setLimitBps(status.limitBps);
        setActive(status.active);
      } catch (error) {
        setProblem(errorText(error));
      }
    })();
  }, [desktop]);

  async function apply(nextEnabled: boolean, nextLimit: number) {
    setBusy(true);
    setProblem(null);
    savePref({ enabled: nextEnabled, limitBps: nextLimit });
    if (!desktop) {
      setEnabled(nextEnabled);
      setLimitBps(nextLimit);
      setBusy(false);
      setAsk(false);
      return;
    }
    try {
      const status = await api.helperSet(nextEnabled, nextLimit);
      setEnabled(status.enabled);
      setLimitBps(status.limitBps);
      setActive(status.active);
      setAsk(false);
    } catch (error) {
      setProblem(errorText(error));
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
              打开后，别人卡在慢公共中继时，可以把密文流量经你转发。默认限速 1 MB/s，可随时关掉。
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            disabled={busy}
            onClick={() => {
              if (enabled) {
                void apply(false, limitBps);
                return;
              }
              setAsk(true);
            }}
            className={`relative h-8 w-14 shrink-0 rounded-full transition ${enabled ? "bg-pine" : "bg-line"}`}
          >
            <span
              className={`absolute top-1 size-6 rounded-full bg-card shadow transition ${enabled ? "left-7" : "left-1"}`}
            />
          </button>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs text-muted">限速</span>
          {limits.map((item) => (
            <button
              key={item.value}
              type="button"
              disabled={busy}
              onClick={() => void apply(enabled, item.value)}
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
          <p className="text-xs leading-5 text-muted">建议仅在有公网 IP，或已经做好端口映射时打开。</p>
        )}
        {problem ? <p className="text-xs text-ember-dark">{problem}</p> : null}
        {!desktop ? <p className="text-xs text-muted">浏览器里只是预览。真正转发要在闪摊窗口里打开。</p> : null}
      </Card>

      {ask ? (
        <div className="fixed inset-0 z-40 grid place-items-center bg-ink/40 px-4" role="dialog" aria-modal="true">
          <div className="w-full max-w-md rounded-[1.4rem] border border-line bg-card p-5 shadow-lg">
            <h4 className="font-serif text-2xl">确认当互助中继？</h4>
            <p className="mt-3 text-sm leading-6 text-muted">
              别人的流量会占用你的上行带宽和公网地址。内容仍是加密的，你看不到文件，但电费和带宽是你的。默认限速 1
              MB/s，可随时关掉。没有公网 IP 时，别人未必找得到你。
            </p>
            <div className="mt-5 flex justify-end gap-2">
              <Button type="button" className="bg-transparent text-ink hover:bg-paper" onClick={() => setAsk(false)}>
                先不要
              </Button>
              <Button type="button" disabled={busy} onClick={() => void apply(true, limitBps)}>
                {busy ? "正在打开" : "打开中继"}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}
