// Quark's coordinator bridge for Pi, written by quarkd into the Project
// workspace and loaded with `pi -e`. Pi has no MCP client, so this registers
// each tool of quarkd's coordinator MCP server as a Pi tool and forwards the
// calls, and reports every finished prompt to the turn-end hook. quarkd
// rewrites this file whenever its URLs change; edits are lost.
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { Type } from "typebox";

const MCP_URL: string = __QUARK_MCP_URL__;
const HOOKS_URL: string = __QUARK_HOOKS_URL__;

let nextId = 1;

async function rpc(method: string, params: unknown, signal?: AbortSignal): Promise<any> {
  const res = await fetch(MCP_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: nextId++, method, params }),
    signal,
  });
  if (!res.ok) throw new Error(`quarkd answered ${res.status} to ${method}`);
  const msg = await res.json();
  if (msg.error) throw new Error(msg.error.message ?? `${method} failed`);
  return msg.result;
}

async function tools(): Promise<any[]> {
  let last: unknown;
  // quarkd may still be starting when the coordinator does.
  for (let attempt = 0; attempt < 10; attempt++) {
    try {
      await rpc("initialize", {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "quark-pi-bridge", version: "1" },
      });
      return (await rpc("tools/list", {})).tools ?? [];
    } catch (e) {
      last = e;
      await new Promise((r) => setTimeout(r, 1000 * (attempt + 1)));
    }
  }
  throw last;
}

export default async function (pi: ExtensionAPI) {
  let listed: any[] = [];
  let failure = "";
  try {
    listed = await tools();
  } catch (e) {
    failure = `Quark's coordinator tools are unavailable: ${e}`;
  }
  for (const tool of listed) {
    pi.registerTool({
      name: tool.name,
      label: tool.title ?? tool.name,
      description: tool.description ?? "",
      parameters: Type.Unsafe(tool.inputSchema ?? { type: "object", properties: {} }),
      async execute(_toolCallId, params, signal) {
        const result = await rpc("tools/call", { name: tool.name, arguments: params ?? {} }, signal);
        const content = (result.content ?? []).filter((c: any) => c.type === "text");
        if (result.isError) {
          throw new Error(content.map((c: any) => c.text).join("\n") || `${tool.name} failed`);
        }
        return { content, details: {} };
      },
    });
  }

  pi.on("session_start", async (_event, ctx) => {
    if (failure) ctx.ui.notify(failure, "error");
  });

  // One finished prompt is one coordinator turn. quarkd reads its token use
  // from Pi's own session log. When the reply asked the user something
  // without ask_user, quarkd answers with a nudge, sent back as a follow-up.
  pi.on("agent_end", async (event: any) => {
    const said = lastReply(event?.messages ?? []);
    try {
      const res = await fetch(`${HOOKS_URL}/turn_end`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(said ? { last_assistant_message: said } : {}),
        signal: AbortSignal.timeout(10_000),
      });
      const answer = res.ok ? await res.json() : {};
      if (answer?.decision === "block" && typeof answer.reason === "string") {
        pi.sendUserMessage(answer.reason, { deliverAs: "followUp" });
      }
    } catch {
      // Not retried: quarkd's wake timeout covers a lost turn end.
    }
  });
}

/// The text of the last assistant message among `messages`.
function lastReply(messages: any[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m?.role !== "assistant") continue;
    const content = m.content;
    const text =
      typeof content === "string"
        ? content
        : (content ?? [])
            .filter((c: any) => c?.type === "text")
            .map((c: any) => c.text)
            .join("\n");
    if (text.trim()) return text;
  }
  return "";
}
