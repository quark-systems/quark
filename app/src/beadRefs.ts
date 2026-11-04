// Bead ids in text people read. An id such as `gluon-e1c` means nothing on its own, so wherever
// daemon or agent text names one of the Project's issues, the app shows the issue's title beside
// it ("gluon-e1c Detect secrets integration"), unless the text already says the title.
import { escapeHtml } from "./markdown";
import { href } from "./nav";

/** What the app needs to name an issue. */
export interface BeadName { id: string; title: string; status?: string; issue_type?: string }
/** A Project's issues by id. */
export type BeadNames = Record<string, BeadName>;
/** Shortened ids an asker used (`e1c`) by the full id they stand for (`gluon-e1c`). */
export type ShortIds = Record<string, string>;

/** A run of plain text, or an issue the text names. */
export type BeadSegment = string | {
  /** The full id. */
  id: string;
  /** The id as the text wrote it: the full id, or a shortened one. */
  shown: string;
  title: string;
  /** False when the surrounding text already says the title, so only the id is marked. */
  withTitle: boolean;
};

// A word made of lowercase letters and digits, hyphen-joined, with optional `.n` child parts:
// `gluon-e1c`, `quark-abc.1`, or a bare `e1c`. Only words that name a known issue are used.
const WORD = /(?<![\w.-])([a-z0-9]+(?:-[a-z0-9]+)*(?:\.\d+)*)(?![\w-])/g;

const norm = (s: string) => s.toLowerCase().replace(/\s+/g, " ").trim();

/** The id's last part, as an asker shortens it: `e1c` for `gluon-e1c`. */
function shortOf(id: string): string | null {
  const i = id.lastIndexOf("-");
  return i > 0 && i < id.length - 1 ? id.slice(i + 1) : null;
}

/**
 * The shortened ids `texts` use for issues they also name in full. "e1c" means `gluon-e1c` only
 * when `gluon-e1c` appears somewhere in the same texts, so ordinary words are never taken for ids.
 */
export function shortIdsIn(texts: (string | null | undefined)[], names: BeadNames): ShortIds {
  const out: ShortIds = {};
  const clash = new Set<string>();
  for (const t of texts) {
    if (!t) continue;
    for (const m of t.matchAll(WORD)) {
      const id = m[1];
      const s = names[id] && shortOf(id);
      if (!s) continue;
      if (out[s] && out[s] !== id) clash.add(s);
      else out[s] = id;
    }
  }
  for (const s of clash) delete out[s];
  return out;
}

/** `text` split into plain runs and the issues it names. */
export function beadSegments(text: string, names: BeadNames, short: ShortIds = {}): BeadSegment[] {
  if (!text) return [text];
  const out: BeadSegment[] = [];
  const said = norm(text);
  let at = 0;
  for (const m of text.matchAll(WORD)) {
    const word = m[1];
    const id = names[word] ? word : short[word];
    const n = id ? names[id] : undefined;
    if (!n) continue;
    if (m.index! > at) out.push(text.slice(at, m.index));
    const title = n.title.trim();
    out.push({ id, shown: word, title, withTitle: !!title && !said.includes(norm(title)) });
    at = m.index! + word.length;
  }
  if (at < text.length) out.push(text.slice(at));
  return out.length ? out : [text];
}

/** True when `text` names at least one known issue. */
export function namesBeads(text: string | null | undefined, names: BeadNames, short: ShortIds = {}): boolean {
  return !!text && beadSegments(text, names, short).some((s) => typeof s !== "string");
}

/** "gluon-e1c: Detect secrets integration" for a known issue, else the id. */
export function beadLabel(id: string, names: BeadNames): string {
  const t = names[id]?.title?.trim();
  return t ? `${id}: ${t}` : id;
}

/** `text` as plain text with each named issue's title beside its id, for tooltips and accessible names. */
export function beadPlain(text: string, names: BeadNames, short: ShortIds = {}): string {
  return beadSegments(text, names, short)
    .map((s) => (typeof s === "string" ? s : s.withTitle ? `${s.shown} (${s.title})` : s.shown))
    .join("");
}

function chipHtml(s: Exclude<BeadSegment, string>, project: string | null | undefined, link: boolean): string {
  const tip = escapeHtml(`${s.id}: ${s.title}`);
  const inner = `<span class="bead-ref-id">${escapeHtml(s.shown)}</span>`
    + (s.withTitle ? `<span class="bead-ref-title">${escapeHtml(s.title)}</span>` : "");
  if (link && project) {
    const to = escapeHtml(href({ name: "issues", project, id: s.id }));
    return `<a class="bead-ref" href="${to}" title="${tip}" data-bead="${escapeHtml(s.id)}">${inner}</a>`;
  }
  return `<span class="bead-ref" title="${tip}" data-bead="${escapeHtml(s.id)}">${inner}</span>`;
}

const unescape = (s: string) => s.replace(/&(amp|lt|gt|quot|#39);/g, (_, e) => ({ amp: "&", lt: "<", gt: ">", quot: '"', "#39": "'" })[e as string]!);

/**
 * Rendered markdown with each issue it names marked up as a bead chip (a link to the issue when
 * `project` is given). Text inside links, code and pre blocks is left alone.
 */
export function annotateBeadsHtml(html: string, names: BeadNames, opts: { project?: string | null; short?: ShortIds; link?: boolean } = {}): string {
  if (!html || !Object.keys(names).length) return html;
  const parts = html.split(/(<[^>]+>)/);
  const plain = unescape(parts.filter((p) => !p.startsWith("<")).join(""));
  let skip = 0;
  return parts.map((p) => {
    if (p.startsWith("<")) {
      const m = /^<(\/?)(a|code|pre)\b/i.exec(p);
      if (m) skip = Math.max(0, skip + (m[1] ? -1 : 1));
      return p;
    }
    if (skip || !p) return p;
    const segs = beadSegments(p, names, opts.short);
    if (segs.length === 1 && typeof segs[0] === "string") return p;
    return segs.map((s) => {
      if (typeof s === "string") return s;
      // Whether the title is already said is judged on the whole text, not this run of it.
      const withTitle = s.withTitle && !norm(plain).includes(norm(s.title));
      return chipHtml({ ...s, withTitle }, opts.project, opts.link ?? true);
    }).join("");
  }).join("");
}
