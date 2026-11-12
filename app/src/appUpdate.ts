// What the app shows about Quark updates: the banner above every screen, the Updates
// screen's summary line, and the shared copy of GET /v1/update both read. quarkd owns
// updates (crates/quarkd/src/update.rs); the app only reports and asks.
import { useSyncExternalStore } from "react";
import { api, NotAvailable, type UpdateStatus } from "./api";
import type { Tone } from "./ui/tone";

export interface UpdateBanner {
  text: string;
  tone: Tone;
  /** The button the banner offers. */
  action?: "install" | "open";
}

/** The banner for `s`, or none when there's nothing to say. A finished install says so
 * once: `seen` is the install (its `started_at`) the person already dismissed. */
export function updateBanner(s: UpdateStatus | null, seen?: string): UpdateBanner | null {
  if (!s) return null;
  const last = s.last_install;
  if (s.state === "installing") {
    const to = last?.to ?? s.offer?.version;
    return { text: `Installing Quark${to ? ` ${to}` : ""}. Quark restarts in a moment.`, tone: "busy" };
  }
  if (last && last.started_at !== seen) {
    if (last.phase === "rolled_back" && last.error) {
      return { text: `Quark ${last.to} couldn't start, so Quark ${last.from} was put back. ${last.error}`, tone: "failed", action: "open" };
    }
    if (last.phase === "failed") {
      return { text: `The update to Quark ${last.to} failed: ${last.error ?? "unknown error"}`, tone: "failed", action: "open" };
    }
  }
  if (s.state === "ready" && s.offer) {
    const why = s.offer.replaces_pulled ? ` It replaces Quark ${s.current_version}, which was withdrawn.` : "";
    return { text: `Quark ${s.offer.version} is ready to install.${why}`, tone: s.offer.replaces_pulled ? "needs-you" : "ready", action: "install" };
  }
  if (s.state === "available" && s.offer) {
    return { text: `Quark ${s.offer.version} is available.`, tone: "ready", action: "open" };
  }
  return null;
}

/** One line for the Updates screen's header. */
export function updateSummary(s: UpdateStatus): string {
  switch (s.state) {
    case "disabled": return s.disabled_reason ?? "This build can't update itself.";
    case "idle": return "Not checked yet.";
    case "checking": return "Checking for updates…";
    case "up_to_date": return s.blocked ?? `Quark ${s.current_version} is up to date.`;
    case "available": return `Quark ${s.offer?.version} is available.`;
    case "downloading": return `Downloading Quark ${s.offer?.version}…`;
    case "ready": return `Quark ${s.offer?.version} is downloaded and ready to install.`;
    case "failed": return s.error ?? "The last check failed.";
    case "installing": return "Installing. Quark restarts in a moment.";
  }
}

// One shared copy of GET /v1/update, refreshed every five minutes, after each action and
// while an install runs (quarkd restarts under it).
let status: UpdateStatus | null = null;
let timer: ReturnType<typeof setTimeout> | undefined;
const listeners = new Set<() => void>();

export function setUpdateStatus(s: UpdateStatus | null) {
  status = s;
  listeners.forEach((l) => l());
  schedule(s?.state === "installing" || s?.state === "checking" || s?.state === "downloading" ? 3_000 : 300_000);
}

function schedule(ms: number) {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => void refreshUpdate(), ms);
}

export async function refreshUpdate() {
  try { setUpdateStatus(await api.update()); }
  catch (e) {
    // A daemon from before updates, or one restarting mid-install: keep the last answer.
    if (e instanceof NotAvailable) { status = null; listeners.forEach((l) => l()); return; }
    schedule(status?.state === "installing" ? 3_000 : 300_000);
  }
}

export function useUpdateStatus(): UpdateStatus | null {
  return useSyncExternalStore(
    (l) => { listeners.add(l); if (listeners.size === 1 && !timer) void refreshUpdate(); return () => listeners.delete(l); },
    () => status,
  );
}

const SEEN = "quark.update.seen";
export function seenInstall(): string | undefined {
  try { return localStorage.getItem(SEEN) ?? undefined; } catch { return undefined; }
}
export function markInstallSeen(startedAt: string) {
  try { localStorage.setItem(SEEN, startedAt); } catch { /* private mode */ }
}
