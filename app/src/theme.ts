// Light/dark theme and window glass. The colors themselves live in styles.css as tokens
// derived from a hue and two lightnesses; this file only flips the classes on <html>:
// - `theme-light` when the resolved scheme is light,
// - `has-native-glass` when the desktop shell gave the window a translucent backing
//   (the Rust side passes `glass=1` in the page URL),
// - `is-mac` on macOS, where the title bar overlays the page.
// Scheme approach adapted from MonoCode (https://github.com/hardbeat920/monocode), MIT,
// Copyright (c) 2026 Nick.

export type ThemePreference = "system" | "light" | "dark";
export type ColorScheme = "light" | "dark";

/** Fired on window with the new scheme whenever it changes (terminals repaint on it). */
export const SCHEME_EVENT = "quark:scheme";
const KEY = "quark.theme";

export function parsePreference(v: string | null | undefined): ThemePreference {
  return v === "light" || v === "dark" ? v : "system";
}

export function resolveScheme(pref: ThemePreference, systemLight: boolean): ColorScheme {
  return pref === "system" ? (systemLight ? "light" : "dark") : pref;
}

/** Reads `glass` and `platform` from the page query the desktop shell builds. */
export function shellFlags(search: string): { glass: boolean; mac: boolean } {
  const q = new URLSearchParams(search);
  return { glass: q.get("glass") === "1", mac: q.get("platform") === "macos" };
}

function systemQuery(): MediaQueryList | null {
  return typeof window !== "undefined" && window.matchMedia ? window.matchMedia("(prefers-color-scheme: light)") : null;
}

export function themePreference(): ThemePreference {
  try { return parsePreference(localStorage.getItem(KEY)); } catch { return "system"; }
}

export function isLight(): boolean {
  return document.documentElement.classList.contains("theme-light");
}

function apply(pref: ThemePreference) {
  const scheme = resolveScheme(pref, systemQuery()?.matches ?? false);
  const root = document.documentElement;
  if (root.classList.contains("theme-light") === (scheme === "light") && root.dataset.scheme) return;
  root.classList.toggle("theme-light", scheme === "light");
  root.dataset.scheme = scheme;
  window.dispatchEvent(new CustomEvent<ColorScheme>(SCHEME_EVENT, { detail: scheme }));
}

export function setThemePreference(pref: ThemePreference) {
  try { localStorage.setItem(KEY, pref); } catch { /* private mode */ }
  apply(pref);
}

/** Applies the stored theme and shell flags; call once before the first render. */
export function initTheme() {
  const root = document.documentElement;
  const { glass, mac } = shellFlags(location.search);
  root.classList.toggle("has-native-glass", glass);
  root.classList.toggle("is-mac", mac);
  const pref = new URLSearchParams(location.search).get("theme");
  apply(pref ? parsePreference(pref) : themePreference());
  systemQuery()?.addEventListener("change", () => apply(themePreference()));
}

/** Resolves a CSS color expression (e.g. `var(--content)`) to a computed color, or null. */
export function cssColor(expr: string): string | null {
  if (typeof document === "undefined" || !document.body) return null;
  const probe = document.createElement("span");
  probe.style.color = expr;
  document.body.appendChild(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color || null;
}

/** A custom property's value on <html>, trimmed, or "" when unset. */
export function cssVar(name: string): string {
  if (typeof document === "undefined") return "";
  return getComputedStyle(document.documentElement).getPropertyValue(name).replace(/\s+/g, " ").trim();
}
