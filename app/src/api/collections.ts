// Collections: named sets of projects the left list groups by. A project may be in several,
// or none. A collection decides nothing about how a project runs — it is what you see.

import { enc, req } from "./client";

export interface Collection {
  id: string; name: string;
  /** Where it sits among the collections, ascending. */
  position: number;
  /** The projects in it, in the order the left list shows them. */
  projects: string[];
  created_at: string; updated_at: string;
}

export interface UpdateCollection {
  name?: string;
  position?: number;
  /** Replaces the whole membership, in the order given. */
  projects?: string[];
}

export const collectionsApi = {
  collections: () => req<Collection[]>("GET", "/v1/collections"),
  createCollection: (name: string) => req<Collection>("POST", "/v1/collections", { name }),
  updateCollection: (id: string, patch: UpdateCollection) =>
    req<Collection>("PATCH", `/v1/collections/${enc(id)}`, patch),
  deleteCollection: (id: string) => req<void>("DELETE", `/v1/collections/${enc(id)}`),
};
