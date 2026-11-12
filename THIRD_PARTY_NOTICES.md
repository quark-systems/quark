# Third-party notices

Quark is MIT licensed (see `LICENSE`). Parts of it are adapted from the projects below, whose notices follow.

## MonoCode

https://github.com/hardbeat920/monocode

Adapted in Quark:

- the design token scheme (one hue and two lightnesses, color-mixed strokes and selection fills, the light theme, diff palettes), motion tokens and popover/modal animations: `app/src/styles.css`, `app/src/theme.ts`;
- the transparent, CSS-driven terminal theme, ANSI palettes and Nerd Font stack: `app/src/term/types.ts`, `app/src/term/xterm.ts`;
- the translucent macOS window with a native blur and overlay title bar: `app/src-tauri/src/lib.rs`;
- tool-call classification and readable titles: `crates/quark-transcript/src/tool.rs`;
- transcript turn structure, work fold, turn footer and scroll anchoring: `app/src/components/transcript/`;
- the unified diff view (file header, counts, folded gaps, word-level marks): `app/src/components/diff/`;
- the session card layout for worker cards: `app/src/components/WorkerCard/WorkerCard.tsx`.

MonoCode's NOTICE disclaims harness and provider trademarks; Quark copies none of MonoCode's logos.

```
MIT License

Copyright (c) 2026 Nick

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## quota-axi

https://github.com/kunchenguid/quota-axi

Adapted in Quark: how each provider's login is found (Keychain items, credential files, Pi's logins), the usage endpoints and their window shapes, and the effective-availability, runway and spend-priority rules, for Claude Code, Codex, Grok, Kimi Code, Cursor, Antigravity, Muse, GitHub Copilot, the key-based providers (OpenRouter, DeepSeek, Z.ai, MiniMax, OpenCode Go, Command Code, MiMo), Alibaba Coding Plan, and Pi's and OpenCode's logins: `crates/quark-dispatch/src/local/` (`claude.rs`, `codex.rs`, `grok.rs`, `kimi.rs`, `cursor.rs`, `agy.rs`, `muse.rs`, `copilot.rs`, `keyed.rs`, `alibaba.rs`, `logins.rs`, `support.rs`, `windows.rs`, `mod.rs`).

```
MIT License

Copyright (c) 2026 Kun Chen

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Simple Icons

https://simpleicons.org, CC0 1.0 Universal (public domain dedication).

Harness logos in `app/src/components/WorkerCard/harnessMarks.ts` are the SVG paths Simple Icons publishes for Claude, Cursor, Google Gemini, Kimi, OpenCode and Pi.
These marks are trademarks of their owners, used only to name which harness a worker runs; Quark is not affiliated with or endorsed by them.
