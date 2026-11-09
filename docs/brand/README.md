# Quark Systems brand marks

Quark Systems makes the tools that run AI software factories: Quark runs the factory floor,
Gluon keeps what it builds true to the architecture. The marks share one idea, the **cell**:
a bounded workspace with one corner cut open where finished work leaves. Every product keeps
the same cell and puts its own figure inside.

| Mark | Figure | Meaning |
| --- | --- | --- |
| Quark Systems | one unit | the factory cell itself |
| Quark | two units at work, a third in the opening | agents doing the work and shipping it |
| Gluon | a closed inner cell holding one unit | work kept inside the rules |

A future tool keeps the cut-corner cell and stroke weight, and adds one interior figure built
from the same parts (units, inner cells, strokes) that says what it does to the work.

## Files

- `marks/<name>-mark.svg`: the mark in `currentColor`, for inline use.
- `marks/<name>-mark-on-dark.svg`, `-on-light.svg`: the mark in its product color for each
  theme.
- `marks/<name>-mark-small.svg`: the drawing for 16–24px (thicker frame, fewer and larger
  figures). Use it for favicons, nav tiles and the menu bar.
- `lockups/<name>-lockup-on-dark.svg`, `-on-light.svg`: mark plus wordmark, with the wordmark
  outlined (IBM Plex Sans SemiBold, lowercase, tracking -0.015em) so no font is needed.
- `app-icons/<name>-app-icon.svg` and `-1024.png`: macOS icon grid (824px tile on a 1024
  canvas). `app/src-tauri/icons/` is generated from the Quark one with
  `npx tauri icon docs/brand/app-icons/quark-app-icon-1024.png`.
- `menubar/quarkbar-template.svg`: QuarkBar's menu bar template image (black on transparent,
  18pt canvas). `mac/Resources/MenuBarIcon.png` and `@2x` are renders of it.

## Color and type

Colors come from the Quark Systems design system tokens: `quark` #4a9cf6 on dark and
#1b66c9 on light, `gluon` #2dd4bf and #0b7a6e, and `ink` #e4e5e8 and #17181c for the parent
mark. Product colors identify a product only and never mean state. Wordmarks are IBM Plex Sans
SemiBold, always lowercase.

## Don'ts

- Don't close the cut corner, round its ends or move it.
- Don't draw the large mark below 24px; switch to the small drawing.
- Don't put a product's figure in another product's color.
