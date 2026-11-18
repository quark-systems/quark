// App-wide preferences: how the app shows what is there. They live on the daemon, so every
// client it serves opens the same way.

import { req } from "./client";

export interface Preferences {
  /** Whether the left list groups projects under their collections. */
  group_by_collection: boolean;
}

export const preferencesApi = {
  preferences: () => req<Preferences>("GET", "/v1/preferences"),
  updatePreferences: (patch: Partial<Preferences>) => req<Preferences>("PATCH", "/v1/preferences", patch),
};
