/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";

// macOS disks are case-insensitive: `overview.ts` beside `Overview.tsx` makes
// `import "./Overview"` load the wrong file there, while Linux (and CI) resolve it fine.
const files = Object.keys(import.meta.glob(["./**/*.ts", "./**/*.tsx", "!./**/*.test.ts", "!./**/*.d.ts"]));

describe("source file names", () => {
  it("never differ only by case or extension in one directory", () => {
    const seen = new Map<string, string>();
    const clashes: string[] = [];
    for (const f of files) {
      const key = f.replace(/\.tsx?$/, "").toLowerCase();
      const other = seen.get(key);
      if (other) clashes.push(`${other} and ${f}`);
      else seen.set(key, f);
    }
    expect(files.length).toBeGreaterThan(50);
    expect(clashes).toEqual([]);
  });
});
