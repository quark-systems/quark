# Third-party notices

Quark is MIT licensed (see `LICENSE`). Parts of it are adapted from the projects below, whose notices follow.

## MonoCode

https://github.com/hardbeat920/monocode

Adapted in Quark:

- the design token scheme (one hue and two lightnesses, color-mixed strokes and selection fills, the light theme, diff palettes), motion tokens and popover/modal animations: `app/src/styles.css`, `app/src/theme.ts`;
- the transparent, CSS-driven terminal theme, ANSI palettes and Nerd Font stack: `app/src/term/types.ts`, `app/src/term/xterm.ts`;
- the translucent macOS window with a native blur and overlay title bar: `app/src-tauri/src/lib.rs`;
- tool-call classification and readable titles: `crates/quark-transcript/src/tool.rs`;
- transcript turn structure, work fold, turn footer and scroll anchoring: `app/src/components/transcript/`.

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
