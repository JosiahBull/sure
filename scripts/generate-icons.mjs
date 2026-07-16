// Generate brand PWA icons (solid teal, rounded) with no image deps — just zlib.
// Writes packages/web/public/icon-192.png and icon-512.png.
import { deflateSync, crc32 } from "node:zlib";
import { writeFileSync } from "node:fs";

const BG = [11, 17, 32]; // --bg
const FG = [45, 212, 191]; // --accent

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0);
  return Buffer.concat([len, body, crc]);
}

function icon(size) {
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
  const onLine = (x, y) => {
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
    const cx = Math.min(Math.max(x, r), size - r);
    const cy = Math.min(Math.max(y, r), size - r);
    return Math.hypot(x - cx, y - cy) <= r + 0.5;
  };

  const raw = Buffer.alloc(size * (size * 4 + 1));
  let o = 0;
  for (let y = 0; y < size; y++) {
    raw[o++] = 0; // filter: none
    for (let x = 0; x < size; x++) {
      const inside = rounded(x, y);
      const [r0, g0, b0] = onLine(x, y) ? FG : BG;
      raw[o++] = r0;
      raw[o++] = g0;
      raw[o++] = b0;
      raw[o++] = inside ? 255 : 0;
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
for (const size of [192, 512]) {
  writeFileSync(new URL(`icon-${size}.png`, dir), icon(size));
  console.log(`wrote icon-${size}.png`);
}
