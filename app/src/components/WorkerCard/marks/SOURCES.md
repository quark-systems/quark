# Harness marks: sources and terms

Each file here is a vendor's own published icon, downloaded on 2026-11-20 and kept byte for byte
(SHA-256 checked against the live file), except where noted. Quark shows a mark only to name which
harness a worker runs (nominative use). The marks are trademarks of their owners, and Quark is not
affiliated with or endorsed by them. Don't recolor, combine or restyle them.

Inline paths in `../harnessMarks.ts` without a file here (Claude Code, Cursor Agent, Gemini CLI, Kimi
Code, OpenCode, Pi, Muse Code) come from [Simple Icons](https://simpleicons.org) under CC0 1.0.

| File | Harness | Source | Terms |
| --- | --- | --- | --- |
| `codex.svg` | Codex | OpenAI brand kit, `OpenAI-logos/SVGs/OAI_OpenAI-Blossom_Black.svg` in https://cdn.openai.com/brand/openai-logos.zip, linked from https://openai.com/brand/ | OpenAI trademark. The brand page allows the logo "only when it directly relates to OpenAI services", used exactly as provided. Codex has no separate published mark, so this is OpenAI's Blossom. The app draws the same path in the text color, cropped to the mark. |
| `kiro.svg` | Kiro CLI | https://kiro.dev/icon.svg (the site's own app icon) | Kiro is an AWS product; Amazon trademark. See https://aws.amazon.com/trademark-guidelines/. |
| `bob.svg` | IBM Bob | https://bob.ibm.com/icon.svg (the site's own app icon) | IBM trademark. See https://www.ibm.com/legal/copytrade.  The original switches palettes with `prefers-color-scheme`, which follows the OS, not Quark's theme. `bob-light.svg` and `bob-dark.svg` are that same file with the media query removed and each of IBM's two palettes fixed; the app shows the one for its theme. |
| `grok.svg` | Grok Build | xAI brand kit, `SpaceXAI_Grok_Assets/Grok_Logomark_Dark.svg` in https://data.x.ai/logos/SpaceXAI_Grok_Assets.zip, linked from https://x.ai/legal/brand-guidelines | Trademark of SpaceXAI (formerly xAI), the developer of Grok. Use only to accurately refer to it or its services, per those guidelines. The app draws the same paths in the text color. |
| `rovo.svg` | Rovo Dev | Atlassian Design System, `@atlaskit/logo` 23.3.3 `rovo-dev/icon` (Apache-2.0 package) | Atlassian trademark. See https://www.atlassian.com/legal/trademark. The SVG string is copied from the package; `xmlns`, `width` and `height` were added so it loads as a file. |
| `antigravity.svg` | Antigravity CLI | Google Antigravity press assets, "Icon - Full Color" SVG: https://antigravity.google/assets/image/brand/antigravity-icon__full-color.svg from https://antigravity.google/press | Google trademark. See https://about.google/brand-resource-center/. |
| `omp.svg` | Oh My Pi | https://github.com/can1357/oh-my-pi/blob/main/assets/icon.svg | MIT (repo license). Drawn for dark backgrounds, so the app puts it on a dark tile. |

Muse Code (Meta) has no mark of its own: its product page https://dev.meta.ai/lp/muse-code shows only
the Meta wordmark, and every Muse extension on the VS Code Marketplace is third-party. Matt chose
(2026-11-20) to show Meta's symbol, drawn from Simple Icons (`meta`, CC0, brand color #0467DF). Meta's
brand page (https://www.meta.com/brand/resources/meta/company-brand/) says all use of the Meta logo
requires Meta's approval; none has been requested. Replace it if Meta publishes a Muse mark (quark#317).
