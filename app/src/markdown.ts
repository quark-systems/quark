import { Marked } from "marked";
import hljs from "highlight.js/lib/core";
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import javascript from "highlight.js/lib/languages/javascript";
import bash from "highlight.js/lib/languages/bash";
import json from "highlight.js/lib/languages/json";
import python from "highlight.js/lib/languages/python";
import diff from "highlight.js/lib/languages/diff";
import go from "highlight.js/lib/languages/go";
import yaml from "highlight.js/lib/languages/yaml";
import toml from "highlight.js/lib/languages/ini";
import css from "highlight.js/lib/languages/css";
import xml from "highlight.js/lib/languages/xml";
import markdown from "highlight.js/lib/languages/markdown";
import "highlight.js/styles/github-dark.css";

for (const [n, l] of Object.entries({ rust, typescript, javascript, bash, json, python, diff, go, yaml, toml, css, xml, markdown })) {
  hljs.registerLanguage(n, l);
}
hljs.registerAliases(["rs"], { languageName: "rust" });
hljs.registerAliases(["ts", "tsx"], { languageName: "typescript" });
hljs.registerAliases(["js", "jsx", "mjs"], { languageName: "javascript" });
hljs.registerAliases(["sh", "shell", "zsh", "console"], { languageName: "bash" });
hljs.registerAliases(["py"], { languageName: "python" });
hljs.registerAliases(["yml"], { languageName: "yaml" });
hljs.registerAliases(["html", "svg"], { languageName: "xml" });
hljs.registerAliases(["md"], { languageName: "markdown" });

export function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

export function highlight(code: string, lang?: string | null): string {
  if (lang && hljs.getLanguage(lang)) {
    try { return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value; } catch { /* fallthrough */ }
  }
  return escapeHtml(code);
}

const EXT: Record<string, string> = {
  rs: "rust", ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", mjs: "javascript",
  sh: "bash", json: "json", py: "python", go: "go", yml: "yaml", yaml: "yaml", toml: "toml",
  css: "css", html: "xml", xml: "xml", md: "markdown",
};
export function langForPath(path: string): string | null {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return EXT[ext] ?? null;
}

const md = new Marked({
  gfm: true,
  breaks: false,
  renderer: {
    code({ text, lang }) {
      const l = (lang ?? "").split(/\s/)[0];
      return `<pre><code class="hljs">${highlight(text, l)}</code></pre>`;
    },
    // Daemon text is untrusted: render raw HTML as text, and keep only web and mail links.
    html({ text }) { return escapeHtml(text); },
    link({ href, title, tokens }) {
      const inner = this.parser.parseInline(tokens);
      if (!/^(https?:|mailto:)/i.test(href)) return inner;
      const t = title ? ` title="${escapeHtml(title)}"` : "";
      return `<a href="${escapeHtml(href)}"${t} target="_blank" rel="noreferrer">${inner}</a>`;
    },
    image({ text }) { return escapeHtml(text); },
  },
});

export function renderMarkdown(src: string): string {
  return md.parse(src, { async: false }) as string;
}
