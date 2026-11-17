// A prompt a harness drew on its screen (a permission request or a menu), answered with a button:
// quarkd picks that choice on the screen. The worker view and the coordinator pane both show it.
import { useState } from "react";
import { api, Decision } from "../api";
import { promptTitle, ScreenPrompt } from "../screenPrompt";
import { cx } from "../ui";
import { errText } from "../util";

export function ScreenPromptCard({ d, p, who }: { d: Decision; p: ScreenPrompt; who: string }) {
  const [busy, setBusy] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const pick = async (i: number) => {
    setBusy(i); setErr(null);
    try { await api.answerDecision(d.id, { answer: String(i + 1), via: "app" }); }
    catch (e) { setErr(errText(e)); setBusy(null); }
  };
  const title = promptTitle(p);
  const detail = p.context.slice(0, -1).join(" ");
  return (
    <div className="screen-prompt" role="group" aria-label={`The ${who} is asking`} data-testid="screen-prompt">
      <div className="sp-q"><span className="sp-who">The {who} is asking</span> {title}</div>
      {detail && <div className="sp-detail mono ellipsis" title={detail}>{detail}</div>}
      <div className="sp-options">
        {p.options.map((o, i) => (
          <button key={i} type="button" className={cx("btn small", i === 0 && "sp-first")} disabled={busy !== null} onClick={() => void pick(i)}>
            {busy === i ? "Choosing…" : o}
          </button>
        ))}
      </div>
      {err && <div className="bad small-text" role="alert">{err}</div>}
    </div>
  );
}
