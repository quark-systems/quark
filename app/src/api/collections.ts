// Collections: named sets of projects the left list groups by. A project may be in several,
// or none. A collection decides nothing about how a project runs — it is what you see.

import { enc, req } from "./client";

export interface Collection {
  id: string; name: string;
  /** Where it sits among the collections, ascending. */
  position: number;
  /** Whether it is folded shut in the left list, showing its heading alone. */
  collapsed: boolean;
  /** The projects in it, in the order the left list shows them. */
  projects: string[];
  created_at: string; updated_at: string;
}

export interface UpdateCollection {
  name?: string;
  position?: number;
  collapsed?: boolean;
  /** Replaces the whole membership, in the order given. */
  projects?: string[];
}

export const collectionsApi = {
  collections: () => req<Collection[]>("GET", "/v1/collections"),
  createCollection: (name: string) => req<Collection>("POST", "/v1/collections", { name }),
  updateCollection: (id: string, patch: UpdateCollection) =>
    req<Collection>("PATCH", `/v1/collections/${enc(id)}`, patch),
  /** `confirmName` must be the collection's name exactly. The projects in it are untouched. */
  deleteCollection: (id: string, confirmName: string) =>
    req<void>("DELETE", `/v1/collections/${enc(id)}`, { confirm_name: confirmName }),
};
