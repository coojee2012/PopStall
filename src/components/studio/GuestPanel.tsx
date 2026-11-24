import { DoorOpen } from "lucide-react";
import { useState } from "react";
import { useSession } from "../../stores/session";
import { StatusDot } from "../common/StatusDot";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { Textarea } from "../ui/textarea";

export function GuestPanel({ embedded = false }: { embedded?: boolean }) {
  const busy = useSession((state) => state.guestBusy);
  const openGuest = useSession((state) => state.openGuest);
  const [shareCode, setShareCode] = useState("");

  const body = (
    <>
      {embedded ? null : (
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="font-serif text-2xl">串门</p>
            <p className="mt-1 text-sm leading-6 text-muted">粘贴分享码。摊位在这一页里打开，不会跳到系统浏览器。</p>
          </div>
          <DoorOpen className="mt-1 size-5 text-ember" />
        </div>
      )}

      <Textarea
        value={shareCode}
        placeholder="endpoint 开头的分享码"
        onChange={(event) => setShareCode(event.target.value)}
        spellCheck={false}
        autoFocus={embedded}
      />

      <div className="flex flex-wrap gap-3">
        <Button onClick={() => void openGuest(shareCode)} disabled={busy}>
          {busy ? "正在打开" : "打开摊位"}
        </Button>
      </div>

      <StatusDot tone={busy ? "busy" : "idle"} label={busy ? "正在接入摊位" : "贴上分享码就能打开"} />
    </>
  );

  if (embedded) {
    return <div className="mt-5 flex flex-col gap-4">{body}</div>;
  }

  return <Card className="flex flex-col gap-5">{body}</Card>;
}
