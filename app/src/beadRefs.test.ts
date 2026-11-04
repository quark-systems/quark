import { describe, expect, it } from "vitest";
import { annotateBeadsHtml, beadLabel, beadPlain, beadSegments, shortIdsIn } from "./beadRefs";
import { blockLabels } from "./decisions";
import { renderMarkdown } from "./markdown";
import type { Decision } from "./api";

const names = {
  "gluon-e1c": { id: "gluon-e1c", title: "Detect secrets integration" },
  "gluon-6kq": { id: "gluon-6kq", title: "Own compose project for control-stack" },
  "gluon-z1e": { id: "gluon-z1e", title: "Fix R6 wording" },
  "gluon-ebt": { id: "gluon-ebt", title: "Event log page" },
  "gluon-ebt.1": { id: "gluon-ebt.1", title: "Event log filters" },
};

describe("beadSegments", () => {
  it("puts the title beside each id it names", () => {
    expect(beadPlain("Start gluon-e1c next.", names)).toBe("Start gluon-e1c (Detect secrets integration) next.");
    expect(beadPlain("then gluon-ebt.1, not gluon-ebt", names)).toBe("then gluon-ebt.1 (Event log filters), not gluon-ebt (Event log page)");
  });
  it("marks only the id when the text already says the title", () => {
    const s = beadSegments("The event log page worker (gluon-ebt)", names);
    expect(s[1]).toMatchObject({ id: "gluon-ebt", withTitle: false });
  });
  it("leaves unknown ids and ordinary hyphenated words alone", () => {
    expect(beadSegments("real-organization checks, gluon-zzz and control-stack", names)).toEqual(["real-organization checks, gluon-zzz and control-stack"]);
  });
  it("reads shortened ids only when the same texts name them in full", () => {
    const short = shortIdsIn(["gluon-e1c: a secret. gluon-6kq: ports.", "e1c + 6kq + z1e"], names);
    expect(short).toEqual({ e1c: "gluon-e1c", "6kq": "gluon-6kq" });
    expect(beadPlain("e1c + 6kq + z1e", names, short)).toBe("e1c (Detect secrets integration) + 6kq (Own compose project for control-stack) + z1e");
    expect(beadPlain("e1c only", names)).toBe("e1c only");
  });
});

describe("annotateBeadsHtml", () => {
  it("links ids in prose to the issue, leaving code and links alone", () => {
    const html = annotateBeadsHtml(renderMarkdown("Do **gluon-e1c** before `gluon-6kq` and [gluon-z1e](https://x.test)."), names, { project: "p1" });
    expect(html).toContain('<a class="bead-ref" href="#/p/p1/issues/gluon-e1c" title="gluon-e1c: Detect secrets integration" data-bead="gluon-e1c"><span class="bead-ref-id">gluon-e1c</span><span class="bead-ref-title">Detect secrets integration</span></a>');
    expect(html).toContain("<code>gluon-6kq</code>");
    expect(html).toContain('<a href="https://x.test" target="_blank" rel="noreferrer">gluon-z1e</a>');
  });
  it("renders chips as spans when links are off", () => {
    expect(annotateBeadsHtml("gluon-e1c", names, { project: "p1", link: false })).toMatch(/^<span class="bead-ref"/);
  });
});

describe("issue names in labels", () => {
  it("names an issue by id and title", () => {
    expect(beadLabel("gluon-e1c", names)).toBe("gluon-e1c: Detect secrets integration");
    expect(beadLabel("gluon-nope", names)).toBe("gluon-nope");
  });
  it("names issues a decision blocks", () => {
    const d = { brief: { blocks: ["gluon-e1c", "https://github.com/o/r/pull/9", "other"] } } as Decision;
    expect(blockLabels(d, {}, names)).toEqual(["gluon-e1c: Detect secrets integration", "PR #9", "other"]);
  });
});
