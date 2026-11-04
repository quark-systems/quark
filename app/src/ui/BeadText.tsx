// Text that may name the Project's issues by id. Each id it names gets the issue's title beside
// it, so nobody has to know what `gluon-e1c` is. See GLOSSARY.md, "Copy rules".
import React, { useMemo } from "react";
import { annotateBeadsHtml, BeadSegment, beadSegments, ShortIds } from "../beadRefs";
import { useBeadNames } from "../beadNames";
import { renderMarkdown, renderMarkdownInline } from "../markdown";
import { href } from "../nav";

function Chip({ s, project, link }: { s: Exclude<BeadSegment, string>; project?: string | null; link: boolean }) {
  const inner = <>
    <span className="bead-ref-id">{s.shown}</span>
    {s.withTitle && <span className="bead-ref-title">{s.title}</span>}
  </>;
  const tip = `${s.id}: ${s.title}`;
  return link && project
    ? <a className="bead-ref" href={href({ name: "issues", project, id: s.id })} title={tip} data-bead={s.id}>{inner}</a>
    : <span className="bead-ref" title={tip} data-bead={s.id}>{inner}</span>;
}

/**
 * Plain text with each issue it names shown as the id and its title. `link` makes each a link to
 * the issue; leave it off inside buttons and links. `short` resolves shortened ids (`e1c`).
 */
export function BeadText({ text, project, short, link = false }: {
  text: string | null | undefined; project: string | null | undefined; short?: ShortIds; link?: boolean;
}) {
  const names = useBeadNames(project);
  const segs = useMemo(() => beadSegments(text ?? "", names, short), [text, names, short]);
  return <>{segs.map((s, i) => (typeof s === "string" ? <React.Fragment key={i}>{s}</React.Fragment>
    : <Chip key={i} s={s} project={project} link={link} />))}</>;
}

/**
 * Markdown (`inline` for one line) with each issue it names shown with its title, as a link to it
 * unless `link` is false (inside a link or button).
 */
export function BeadMarkdown({ text, project, short, inline = false, link = true, as: Tag = inline ? "span" : "div", className, ...rest }: {
  text: string; project: string | null | undefined; short?: ShortIds; inline?: boolean; link?: boolean;
  as?: "div" | "span" | "h2" | "p"; className?: string;
} & Omit<React.HTMLAttributes<HTMLElement>, "dangerouslySetInnerHTML" | "className">) {
  const names = useBeadNames(project);
  const html = useMemo(() => annotateBeadsHtml(inline ? renderMarkdownInline(text) : renderMarkdown(text), names, { project, short, link }),
    [text, names, project, short, inline, link]);
  return <Tag className={className} {...rest} dangerouslySetInnerHTML={{ __html: html }} />;
}
