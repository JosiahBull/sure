// Category colour. One hue family per top-level category, shaded by how deep in that family
// a node sits — so a whole branch reads as one colour and the levels within it stay
// distinguishable. Split out of api.ts so the money-flow chart, the pies, the Categories
// page and the transaction row pill all derive a colour the same way instead of each
// guessing at it.
//
// The previous app went further and *stored* the parent's colour on every child
// (`Category#inherit_color_from_parent`), so a parent and child rendered identically. That
// is fine at two levels and unreadable at three, which is why the shade is computed here
// from the depth rather than flattened into the row.

/** Ported from the previous app's `Category::COLORS`, so the families stay recognisable. */
const PALETTE = [
  "#e99537", "#4da568", "#6471eb", "#db5a54", "#df4e92",
  "#c44fe9", "#eb5429", "#61c9ea", "#805dee", "#6ad28a",
];

/** Deterministic color for a category id (stable across renders). */
export function colorFor(key: string | number | null | undefined, fallbackIndex = 0): string {
  if (key === null || key === undefined) return "#737373"; // Category::UNCATEGORIZED_COLOR
  const s = String(key);
  let h = fallbackIndex;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return PALETTE[h % PALETTE.length];
}

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

function toHsl(hex: string): { h: number; s: number; l: number } {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return { h: 0, s: 0, l: 0.5 };
  const n = parseInt(m[1], 16);
  const r = ((n >> 16) & 255) / 255;
  const g = ((n >> 8) & 255) / 255;
  const b = (n & 255) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = d / (1 - Math.abs(2 * l - 1));
  const h =
    max === r ? 60 * (((g - b) / d + 6) % 6) : max === g ? 60 * ((b - r) / d + 2) : 60 * ((r - g) / d + 4);
  return { h, s, l };
}

function toHex({ h, s, l }: { h: number; s: number; l: number }): string {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = l - c / 2;
  const [r, g, b] =
    h < 60 ? [c, x, 0]
    : h < 120 ? [x, c, 0]
    : h < 180 ? [0, c, x]
    : h < 240 ? [0, x, c]
    : h < 300 ? [x, 0, c]
    : [c, 0, x];
  const hx = (v: number) => Math.round(clamp01(v + m) * 255).toString(16).padStart(2, "0");
  return `#${hx(r)}${hx(g)}${hx(b)}`;
}

/**
 * Move a colour `steps` shades *away from the page background*: lighter on the dark theme,
 * deeper on the light one. Either way, further down the hierarchy means further from the
 * surface behind it, so contrast goes up rather than down in both palettes.
 */
export function shade(hex: string, steps: number, dark: boolean): string {
  if (steps <= 0) return hex;
  const { h, s, l } = toHsl(hex);
  return toHex({
    h,
    // Pull a little saturation out as it lightens, so a third-level tint doesn't go neon.
    s: clamp01(dark ? s - 0.06 * steps : s),
    l: clamp01(l + (dark ? 1 : -1) * 0.11 * steps),
  });
}

/** A category node's fill: its family's base colour, shaded by its depth in that family. */
export function categoryColor(opts: {
  rootId: number | null | undefined;
  rootColor?: string | null;
  depth: number;
  dark: boolean;
}): string {
  return shade(opts.rootColor ?? colorFor(opts.rootId ?? null), opts.depth, opts.dark);
}
