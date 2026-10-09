// Choosing a folder on disk. The desktop app opens the system's folder dialog; a path only
// means something to the daemon if it runs on this machine, so a remote daemon gets none.
import { daemonUrl } from "./api";

type Picker = (title: string, defaultPath?: string) => Promise<string | null>;

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    /** Test seam: e2e runs in a plain browser and stands in for the system dialog with this. */
    __quarkPickFolder?: Picker;
  }
}

/** True when the daemon listens on this machine, so a folder picked here is a folder it can open. */
export function daemonIsLocal(url = daemonUrl()): boolean {
  try {
    const h = new URL(url).hostname;
    return h === "127.0.0.1" || h === "localhost" || h === "[::1]" || h === "::1";
  } catch {
    return false;
  }
}

function picker(): Picker | null {
  if (typeof window === "undefined") return null;
  if (window.__quarkPickFolder) return window.__quarkPickFolder;
  if (!window.__TAURI_INTERNALS__) return null;
  return async (title, defaultPath) => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const r = await open({ directory: true, multiple: false, title, defaultPath });
    return typeof r === "string" ? r : null;
  };
}

/** Whether a folder can be chosen with the system dialog here, and if not, why. */
export function folderPicking(): { ok: true } | { ok: false; reason: "browser" | "remote_daemon" } {
  if (!picker()) return { ok: false, reason: "browser" };
  if (!daemonIsLocal()) return { ok: false, reason: "remote_daemon" };
  return { ok: true };
}

/** Opens the system folder dialog; null when cancelled or unavailable. */
export async function pickFolder(title: string, defaultPath?: string): Promise<string | null> {
  const p = picker();
  return p ? p(title, defaultPath) : null;
}
