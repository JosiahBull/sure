# Layout

How the SPA lays itself out from a 320px phone to a wide desktop, and the one rule that makes
the rest of it work. If you are adding a card, a table or a row of buttons, the short version is:

> **Size page content with `@container main (...)`, not `@media (...)`.** The window's width and
> the width your card actually gets are different numbers, and only one of them is the one you
> mean.

## The three regions

The shell (`src/App.svelte`) is a rail, a panel and a content column:

| | wide (≥ `--bp-dock`) | narrow | phone (≤ `--bp-phone`) |
| --- | --- | --- | --- |
| **rail** — Dashboard / Transactions / Settings | 80px column, left | 80px column, left | bottom tab bar |
| **panel** — the account list, or the settings menu | docked 300px column | overlay drawer | overlay drawer |
| **content** | the rest | the rest | the whole width |

Both breakpoints are declared, with their reasoning, at the top of `src/app.css`:

- **`--bp-phone: 720px`** — below it the rail becomes a bottom tab bar. 80px of permanent left
  rail is a fifth of a 402px screen, and the bottom of the screen is where a thumb already is.
- **`--bp-dock: 1000px`** — below it the panel stops taking a column and overlays the content as
  a modal drawer. 80 (rail) + 300 (panel) + a content column worth having (~620) is 1000px.

CSS cannot interpolate a custom property into a media query, so each number is restated at the
rules that use it, and `--bp-dock` is restated a third time in `App.svelte`'s `matchMedia` call.
`tests/mobile.spec.ts` checks the restatements against the declarations, so a change that misses
one fails the suite instead of drifting.

## Why container queries

`.main-col` is a [container](https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_containment/Container_queries):

```css
.main-col {
  container: main / inline-size;
}
```

Everything inside it — every card, table and row — asks how wide *that column* is:

```css
@container main (max-width: 760px) {
  .two {
    grid-template-columns: 1fr;
  }
}
```

This is not a stylistic preference. Before it, the page's breakpoints were viewport-relative
while the content lived in a column of `viewport − 380px`, and the two disagreed most exactly
where a layout is hardest: **the tablet was the worst-laid-out width in the app.** At a 768px
viewport the docked panel left a 382px content column — narrower than the phone the phone rules
were written for — but `@media (max-width: 720px)` did not match, so the dashboard rendered two
columns into 382px and overflowed the document by 416px. A 1024px window was still 167px over.
Measured, not theorised; `@container` is what closed it, at every width at once.

Use a viewport `@media` query only for things that are genuinely about the *screen*: the tab bar
and its safe-area inset, anything that dodges it (`.bulkbar`), and the drawer.

## Nothing is wider than the screen

`body` carries `overflow-x: hidden`, because a phone has no horizontal scrollbar to warn you —
the page just drifts sideways under a thumb, sliding content out from under the sticky bars. That
is a backstop, not a fix: it hides overflow rather than preventing it, so treat any of it as a
bug. `tests/mobile.spec.ts` fails the build if the document is wider than the viewport on any
route.

Two things cause almost all of it:

- **A grid or flex item's `min-width` defaults to `auto`** — "never narrower than my
  min-content". One stubborn descendant then widens its whole track, and since tracks stretch,
  every sibling grows with it. `.grid > *` carries `min-width: 0` globally for this reason; add
  it to flex children yourself (`.row` items with text that should ellipsise need it too).
- **A control that will not shrink** — a `<select>` is as wide as its longest option, and a
  money figure does not wrap. Give the row somewhere to go (`flex-wrap`) rather than hoping.

When something genuinely must be wider than its column, it scrolls itself with `.scroll-x`
(`app.css`) — never the page. The wide tables use it.

## Fixed elements: never trust `bottom: 0` / `right: 0` on a phone

**A fixed element resolves its offsets against the *layout* viewport, and on a phone that is
bigger than the part you can see** whenever the browser's toolbars are up. Anything pinned with
`bottom: 0` or `right: 0` is therefore placed relative to an edge that is off the screen.

This cost the app its entire primary navigation. At 320×568 the layout viewport is **365×648**:
the bottom tab bar was laid out at `y = 582..648` and 365px wide, so it sat below the fold *and*
ran 45px past the right edge. There was no way to change pages at all — and only on small
screens, so it survived a pass that checked 402×874.

Viewport *units* do track what you can see: in that same case `vh`/`dvh`/`svh`/`lvh` all report
568 and `vw`/`dvw`/`svw` all report 320. So size and place against those:

```css
.rail {
  bottom: 0;                    /* fallback for anything too old to know dvh */
  right: 0;
  top: calc(100dvh - var(--tabbar-h) - env(safe-area-inset-bottom));
  width: 100dvw;                /* a specified top/width wins over bottom/right */
}
```

`dvh` rather than `svh` so the bar follows the toolbars as they collapse, staying flush to the
bottom instead of floating above a gap. Every fixed element in the app is built this way — the
tab bar, the drawer, its scrim, and the transactions bulk-action bar. The bulk bar's height
varies with how many of its controls have wrapped, so it uses `top` + `translateY(-100%)` rather
than a computed offset.

`env(safe-area-inset-*)` is a *separate* concern and does not help with this one: it describes
the notch and the home indicator, not the browser's chrome.

Related, and the same family of mistake: a **sticky** bar is not subtracted from the scrollport,
so anything the browser scrolls into view — a `?tx=` deep link, find-in-page, tabbing to an
off-screen field — lands underneath it. `html` carries `scroll-padding-top`/`-bottom` for both
bars; the sticky sub-bar's height is measured and published as `--subbar-h` by `App.svelte`,
because it is one row or two depending on the page.

## Touch

`@media (pointer: coarse)` in `app.css` raises controls to the 44px both platform guidelines ask
for. It keys on the *input device*, not on a width: a 500px browser window on a laptop still has
a mouse, and widening its buttons would spend space on nothing.

Where a control's drawn size is load-bearing for a dense row, the hit area grows and the drawing
does not — `.btn-sm` gets 40px of real height plus a centred pseudo-element topping it up to 44.
It is 40px and not 32px+phantom because an invisible hit area works for a lone button and fails
for a row of them: an account row is `[Edit][Import][×]`, and three 44px phantoms around three
33px buttons overlap, so whichever is painted last wins the shared strip.

One more thing that rule buys: form fields get `font-size: max(16px, 1em)`. Under 16px, iOS
Safari zooms the viewport to the focused field and never zooms back, which is the single most
common way a form "breaks" on an iPhone.

## Where the phone-specific pieces live

| piece | file |
| --- | --- |
| bottom tab bar, drawer, scrim, sub-bar wrapping | `src/App.svelte` |
| tokens, breakpoints, touch rules, `.scroll-x`, scroll lock | `src/app.css` |
| the stacked transaction row (amount on the first line) | `src/pages/Transactions.svelte` |
| the donut that yields width to its legend | `src/pages/Dashboard.svelte`, `src/lib/charts/PieChart.svelte` |

The regression tests for all of it are `packages/web/tests/mobile.spec.ts`; the whole browser
suite already runs at 402×874 (see [TESTING.md](TESTING.md)).
