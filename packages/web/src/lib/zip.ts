// Build a zip in the browser, so picking several files is one upload.
//
// Every importer already accepts a zip — that is how a myIR upload has to arrive, because the
// cross-file gap check can only run with every export in hand — but until now "several files"
// meant the person made the archive themselves. This does it for them, and deliberately changes
// nothing on the server: the bytes that arrive are the same shape as a hand-made zip, so the
// hardened entry/byte budget in `sure_providers::zipfile` and every zip-bomb test keep covering
// this path exactly as they cover that one.
//
// STORE only (no compression). A bank export is a few hundred kilobytes and the request cap is
// 16 MiB, so there is nothing to gain, and deflate in the browser would mean either a dependency
// or hand-rolled Huffman coding. Ported from `packages/api-tests/helpers.ts`'s `makeZip`, which
// is the same writer against Node's `Buffer`.

/** CRC-32, computed per entry — the zip format stores it for the *uncompressed* bytes. */
function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    let c = (crc ^ bytes[i]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

const LOCAL_HEADER = 0x04034b50;
const CENTRAL_HEADER = 0x02014b50;
const END_OF_CENTRAL = 0x06054b50;
/** The version-needed-to-extract for a plain stored entry. */
const VERSION = 20;

type Entry = { name: Uint8Array; body: Uint8Array; crc: number; offset: number };

/**
 * A stored zip of `files`, in the order given.
 *
 * Names are used verbatim: they become the file names an import reports back, so a preview can
 * say which of the person's downloads a row came from.
 */
export function makeZip(files: { name: string; body: Uint8Array }[]): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const entries: Entry[] = [];
  let localSize = 0;
  for (const file of files) {
    const name = encoder.encode(file.name);
    entries.push({ name, body: file.body, crc: crc32(file.body), offset: localSize });
    localSize += 30 + name.length + file.body.length;
  }
  const centralSize = entries.reduce((n, e) => n + 46 + e.name.length, 0);

  const out = new Uint8Array(localSize + centralSize + 22);
  const view = new DataView(out.buffer);
  let at = 0;

  for (const e of entries) {
    view.setUint32(at, LOCAL_HEADER, true);
    view.setUint16(at + 4, VERSION, true);
    // Method 0 (stored), so the compressed and uncompressed sizes are both the body's length.
    view.setUint32(at + 14, e.crc, true);
    view.setUint32(at + 18, e.body.length, true);
    view.setUint32(at + 22, e.body.length, true);
    view.setUint16(at + 26, e.name.length, true);
    out.set(e.name, at + 30);
    out.set(e.body, at + 30 + e.name.length);
    at += 30 + e.name.length + e.body.length;
  }

  const centralAt = at;
  for (const e of entries) {
    view.setUint32(at, CENTRAL_HEADER, true);
    view.setUint16(at + 4, VERSION, true);
    view.setUint16(at + 6, VERSION, true);
    view.setUint32(at + 16, e.crc, true);
    view.setUint32(at + 20, e.body.length, true);
    view.setUint32(at + 24, e.body.length, true);
    view.setUint16(at + 28, e.name.length, true);
    view.setUint32(at + 42, e.offset, true);
    out.set(e.name, at + 46);
    at += 46 + e.name.length;
  }

  view.setUint32(at, END_OF_CENTRAL, true);
  view.setUint16(at + 8, entries.length, true);
  view.setUint16(at + 10, entries.length, true);
  view.setUint32(at + 12, centralSize, true);
  view.setUint32(at + 16, centralAt, true);
  return out;
}

/**
 * One request body from whatever was picked.
 *
 * A single file goes as-is, which keeps the ordinary case byte-for-byte what it has always been.
 * Several are wrapped.
 *
 * A `.zip` among several is refused rather than nested: `sure_providers::asb` deliberately
 * declines a zip inside a zip (a hostile shape), so nesting would produce an upload the server
 * is right to reject and the person would have no way to understand. Asking them to import the
 * archive on its own is the honest answer, and it already works.
 */
export async function uploadBody(
  files: File[],
): Promise<{ body: Blob | File; contentType: string } | { error: string }> {
  if (files.length === 0) return { error: "Pick a file to import." };
  if (files.length === 1) return { body: files[0], contentType: contentTypeOf(files[0]) };

  const archive = files.find((f) => isZip(f));
  if (archive) {
    return {
      error: `${archive.name} is already an archive — import it on its own, or pick only the files inside it.`,
    };
  }
  const bodies = await Promise.all(
    files.map(async (f) => ({ name: f.name, body: new Uint8Array(await f.arrayBuffer()) })),
  );
  return { body: new Blob([makeZip(bodies)]), contentType: "application/zip" };
}

const isZip = (f: File) => f.name.toLowerCase().endsWith(".zip");

/**
 * Advisory only — every importer tells the formats apart by content — but sending the truth keeps
 * the request honest, and keeps a proxy in between from guessing.
 */
function contentTypeOf(file: File): string {
  const name = file.name.toLowerCase();
  if (name.endsWith(".zip")) return "application/zip";
  if (name.endsWith(".csv")) return "text/csv";
  if (name.endsWith(".xlsx"))
    return "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
  return "application/octet-stream";
}
