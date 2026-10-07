import { describe, expect, it } from "vitest";
import { changedRange, markRange, parseUnifiedDiff, splitPath } from "./model";

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

describe("file status and gaps", () => {
  it("reads added, deleted, renamed and binary files", () => {
    const files = parseUnifiedDiff([
      "diff --git a/old.txt b/new.txt", "similarity index 90%", "rename from old.txt", "rename to new.txt",
      "diff --git a/gone.rs b/gone.rs", "deleted file mode 100644", "--- a/gone.rs", "+++ /dev/null", "@@ -1 +0,0 @@", "-x",
      "diff --git a/logo.png b/logo.png", "Binary files a/logo.png and b/logo.png differ",
    ].join("\n"));
    expect(files.map((f) => [f.path, f.oldPath, f.status, f.binary])).toEqual([
      ["new.txt", "old.txt", "renamed", false], ["gone.rs", "gone.rs", "deleted", false], ["logo.png", "logo.png", "modified", true],
    ]);
  });

  it("counts the unchanged lines between hunks and keeps the section name", () => {
    const [f] = parseUnifiedDiff([
      "diff --git a/x b/x", "--- a/x", "+++ b/x",
      "@@ -4,2 +4,2 @@ fn first()", " a", "-b", "+B",
      "@@ -20,1 +20,1 @@", "-c", "+C",
    ].join("\n"));
    const hunks = f.lines.filter((l) => l.kind === "hunk");
    expect(hunks.map((h) => [h.hidden, h.section])).toEqual([[3, "fn first()"], [14, undefined]]);
    expect(f.status).toBe("modified");
  });
});

describe("word changes", () => {
  it("marks the differing middle of paired lines", () => {
    const [f] = parseUnifiedDiff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-let total = old(a);\n+let total = fresh(a);\n");
    const [del, add] = f.lines.slice(1);
    expect(del.text.slice(...del.word!)).toBe("old");
    expect(add.text.slice(...add.word!)).toBe("fresh");
  });

  it("leaves rewritten lines whole", () => {
    expect(changedRange("alpha beta", "zzzz")).toBeNull();
    expect(changedRange("same", "same")).toBeNull();
  });

  it("wraps a range across highlighter tags and entities", () => {
    const html = '<span class="k">let</span> a &amp;&amp; b';
    // text is "let a && b"; mark "t a &"
    expect(markRange(html, 2, 7, "w")).toBe('<span class="k">le<span class="w">t</span></span><span class="w"> a &amp;</span>&amp; b');
    expect(markRange("abc", 0, 3, "w")).toBe('<span class="w">abc</span>');
  });

  it("splits a path into folder and name", () => {
    expect(splitPath("src/a/b.ts")).toEqual({ dir: "src/a/", base: "b.ts" });
    expect(splitPath("README.md")).toEqual({ dir: "", base: "README.md" });
  });
});
