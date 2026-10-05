// Ordering and lookups for the Memory screen, kept pure so they are easy to test.
import type { MemoryEntry, MemoryProposal, UserMemoryEntry } from "./api";

/** A Project's proposals awaiting review, oldest first, like open decisions. */
export function pendingProposals(all: MemoryProposal[], projectId: string): MemoryProposal[] {
  return all.filter((m) => m.project_id === projectId && m.state === "proposed")
    .sort((a, b) => a.proposed_at.localeCompare(b.proposed_at) || a.id.localeCompare(b.id));
}

/** Entries newest first; hand-written files, which carry no date, come last by path. */
export function sortEntries(entries: MemoryEntry[]): MemoryEntry[] {
  const when = (e: MemoryEntry) => e.accepted_at ?? e.date ?? "";
  return [...entries].sort((a, b) => when(b).localeCompare(when(a)) || a.path.localeCompare(b.path));
}

/** The user-level copy of a Project's entry, when it was promoted. */
export function promotedCopy(shared: UserMemoryEntry[], e: MemoryEntry): UserMemoryEntry | undefined {
  return shared.find((u) => u.project_id === e.project_id && u.entry_id === e.id);
}

/** `owner/repo#12` for a forge pull request URL; the URL itself when it has another shape. */
export function prLabel(url: string): string {
  const m = /^https?:\/\/[^/]+\/([^/]+\/[^/]+)\/pull\/(\d+)/.exec(url);
  return m ? `${m[1]}#${m[2]}` : url;
}
