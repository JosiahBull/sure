// Generate brand PWA icons (solid teal, rounded) with no image deps — just zlib.
// Writes four PNGs into packages/web/public, because three consumers want three shapes of the
// same mark:
//
//   icon-192.png, icon-512.png  the mark as drawn: a rounded tile with transparent corners.
//                               Android and desktop installs; iOS only if there were no
//                               apple-touch-icon.
//   apple-touch-icon.png        180x180, the size every current iPhone asks for. Square and
//                               opaque: iOS cuts its own rounded mask, and fills transparency
//                               with black first, so drawn corners would show as dark slivers
//                               just inside its curve.
//   icon-maskable-512.png       square and opaque, with the mark shrunk into the central circle
//                               of radius 40% that a maskable icon guarantees survives any
//                               launcher's crop. The drawn line reaches 52% of the way out, so
//                               reusing icon-512.png here lost both of its ends to a circle.
import { deflateSync, crc32 } from "node:zlib";
import { writeFileSync } from "node:fs";

// The tile and the line. The tile was the palette's --bg when this was drawn; the palette has
// since moved to neutral greys, and the mark kept its colours.
const BG = [11, 17, 32];
const FG = [45, 212, 191];

// Samples per pixel along each axis. Without them every edge is a staircase, which on a 3x
// screen showing a 180px icon at full size is visible.
const SS = 4;

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0);
  return Buffer.concat([len, body, crc]);
}

/**
 * @param {number} size
 * @param {{ square?: boolean, safeZone?: boolean }} opts `square` fills the whole image with the
 *   tile; `safeZone` also shrinks the mark about the centre until it fits the maskable circle.
 */
function icon(size, { square = false, safeZone = false } = {}) {
  const r = size * 0.22; // corner radius
  const inset = size * 0.16;
  const stroke = size * 0.075;
  // A simple upward "trend" polyline through 4 points.
  const pts = [
    [inset, size - inset - size * 0.06],
    [size * 0.42, size * 0.52],
    [size * 0.58, size * 0.62],
    [size - inset, inset],
  ];
  const c = size / 2;
  // The furthest a polyline gets from a point is at one of its vertices; add the stroke's half.
  const reach = Math.max(...pts.map(([x, y]) => Math.hypot(x - c, y - c))) + stroke / 2;
  // 95% of the safe radius, so the stroke's antialiased edge stays inside it too.
  const scale = safeZone ? (0.4 * size * 0.95) / reach : 1;

  const onLine = (x, y) => {
    // Sample the unscaled mark at the point that scaling would have moved here.
    x = c + (x - c) / scale;
    y = c + (y - c) / scale;
    for (let i = 0; i < pts.length - 1; i++) {
      const [x1, y1] = pts[i];
      const [x2, y2] = pts[i + 1];
      const dx = x2 - x1;
      const dy = y2 - y1;
      const len2 = dx * dx + dy * dy;
      let t = ((x - x1) * dx + (y - y1) * dy) / len2;
      t = Math.max(0, Math.min(1, t));
      const px = x1 + t * dx;
      const py = y1 + t * dy;
      if (Math.hypot(x - px, y - py) <= stroke / 2) return true;
    }
    return false;
  };
  const rounded = (x, y) => {
    if (square) return true;
    const cx = Math.min(Math.max(x, r), size - r);
    const cy = Math.min(Math.max(y, r), size - r);
    return Math.hypot(x - cx, y - cy) <= r + 0.5;
  };

  const raw = Buffer.alloc(size * (size * 4 + 1));
  let o = 0;
  for (let y = 0; y < size; y++) {
    raw[o++] = 0; // filter: none
    for (let x = 0; x < size; x++) {
      // Coverage of the tile, and of the line within it, over SS x SS points in the pixel.
      let tile = 0;
      let line = 0;
      for (let j = 0; j < SS; j++) {
        for (let i = 0; i < SS; i++) {
          const sx = x + (i + 0.5) / SS;
          const sy = y + (j + 0.5) / SS;
          if (!rounded(sx, sy)) continue;
          tile++;
          if (onLine(sx, sy)) line++;
        }
      }
      // Straight (not premultiplied) colour: the mix of the two inks over the covered part.
      const mix = tile ? line / tile : 0;
      for (let ch = 0; ch < 3; ch++) raw[o++] = Math.round(BG[ch] + (FG[ch] - BG[ch]) * mix);
      raw[o++] = Math.round((255 * tile) / (SS * SS));
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type RGBA
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  return png;
}

const dir = new URL("../packages/web/public/", import.meta.url);
const icons = [
  ["icon-192.png", icon(192)],
  ["icon-512.png", icon(512)],
  ["apple-touch-icon.png", icon(180, { square: true })],
  ["icon-maskable-512.png", icon(512, { square: true, safeZone: true })],
];
for (const [file, png] of icons) {
  writeFileSync(new URL(file, dir), png);
  console.log(`wrote ${file}`);
}
