import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";

const DIFF = `diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
-    old();
+    new();
+    more();
 }
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,1 @@
+# New
\\ No newline at end of file
`;

describe("parseUnifiedDiff", () => {
  it("splits files and counts changes", () => {
    const files = parseUnifiedDiff(DIFF);
    expect(files.map((f) => [f.path, f.adds, f.dels])).toEqual([["src/a.rs", 2, 1], ["docs/new.md", 1, 0]]);
  });

  it("numbers old and new lines", () => {
    const [a] = parseUnifiedDiff(DIFF);
    const lines = a.lines.filter((l) => l.kind !== "hunk").map((l) => [l.kind, l.old, l.new]);
    expect(lines).toEqual([["ctx", 1, 1], ["del", 2, undefined], ["add", undefined, 2], ["add", undefined, 3], ["ctx", 3, 4]]);
  });

  it("keeps a removed line that starts with dashes inside a hunk", () => {
    const [f] = parseUnifiedDiff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,1 +1,0 @@\n--- not a header\n");
    expect(f.dels).toBe(1);
    expect(f.lines[1]).toMatchObject({ kind: "del", text: "-- not a header" });
  });
});
