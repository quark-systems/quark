// Links that leave the app. The desktop app hands them to the system's default browser through
// the shell's opener; in a plain browser they open in a new tab. Only web addresses are opened.

/** The URL when it is an http or https address, else null. */
export function webUrl(url: string | null | undefined): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    return u.protocol === "https:" || u.protocol === "http:" ? u.href : null;
  } catch {
    return null;
  }
}

type Opener = (url: string) => Promise<void>;

declare global {
  interface Window {
    /** Test seam: e2e stands in for the desktop shell's opener with this. */
    __quarkOpenUrl?: Opener;
  }
}

function opener(): Opener {
  if (typeof window !== "undefined" && window.__quarkOpenUrl) return window.__quarkOpenUrl;
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    return async (url) => {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
    };
  }
  return async (url) => { window.open(url, "_blank", "noopener,noreferrer"); };
}

/** Opens `url` in the default browser. Returns false, opening nothing, when it is not a web address. */
export async function openExternal(url: string | null | undefined): Promise<boolean> {
  const u = webUrl(url);
  if (!u) return false;
  await opener()(u);
  return true;
}

/**
 * Props for an `<a>` that leaves the app: the address stays on `href` (hover, copy link), and a
 * plain click goes through {@link openExternal}. A modified click is left to the browser.
 */
export function externalLink(url: string) {
  return {
    href: url,
    target: "_blank",
    rel: "noreferrer",
    onClick: (e: { preventDefault(): void; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; button: number }) => {
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return;
      e.preventDefault();
      void openExternal(url);
    },
  };
}
