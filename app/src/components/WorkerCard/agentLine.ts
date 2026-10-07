// The model and branch line on a worker card.

/** A model id as a card shows it: no provider or `claude-` prefix, no release date or context-window suffix. */
export function shortModel(model: string): string {
  let m = model.trim();
  m = m.split("/").pop() ?? m;
  return m
    .replace(/^anthropic\./, "")
    .replace(/\[[^\]]*\]$/, "")
    .replace(/-\d{8}$/, "")
    .replace(/^claude-/, "");
}
