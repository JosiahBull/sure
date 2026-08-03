import net from "node:net";
import { expect } from "@playwright/test";
import { deflateRawSync } from "node:zlib";
import type { Schemas, SureClient } from "../client/src/index";

type AccountKind = Schemas["SaveAccount"]["kind"];
type CategoryKind = NonNullable<Schemas["SaveCategory"]["kind"]>;

/**
 * The metadata each kind now *requires* — see `AccountMetadata::validate_for` in sure-core.
 * Kinds absent here need none: our `kind` already says what a depository subtype would, and
 * "other asset"/"other liability" are deliberately free-form.
 *
 * Values are only there to be valid; a spec that cares about a field passes its own, which
 * wins over these (see {@link createAccount}).
 */
const REQUIRED_METADATA: Partial<Record<AccountKind, Record<string, unknown>>> = {
  real_estate: {
    profile: "property",
    subtype: "single_family_home",
    address_line1: "12 Rimu Street",
    city: "Wellington",
    country: "New Zealand",
  },
  vehicle: { profile: "vehicle", make: "Toyota", model: "RAV4", year: 2021 },
  // A mortgage/loan carries its whole amortisation schedule: term and start date make the
  // payoff projectable, and a fixed rate must also say what to assume once it expires.
  mortgage: {
    profile: "mortgage",
    lender: "ASB",
    original_amount_minor: 48_500_000,
    interest_rate_bps: 549,
    rate_type: "fixed",
    fixed_until: "2027-01-11",
    refix_rate_bps: 549,
    refix_rate_uncertainty_bps: 150,
    term_months: 360,
    start_date: "2024-01-01",
  },
  // Floating, so it needs no refix terms — the fixed case is covered by `mortgage`.
  loan: {
    profile: "loan",
    subtype: "other",
    lender: "MTF Finance",
    original_amount_minor: 1_500_000,
    interest_rate_bps: 890,
    rate_type: "floating",
    term_months: 60,
    start_date: "2024-01-01",
  },
  student_loan: {
    profile: "loan",
    subtype: "student",
    lender: "StudyLink",
    original_amount_minor: 3_000_000,
    interest_rate_bps: 0,
  },
  // Only the revolving kinds carry a limit.
  credit_card: { profile: "depository", credit_limit_minor: 1_000_000 },
  revolving_credit: { profile: "depository", credit_limit_minor: 5_000_000 },
  // A listed holding is priced by (ticker, exchange); an unlisted one has neither.
  shares_nz: { profile: "shares", broker: "Sharesies", ticker: "MEL", exchange: "NZX" },
  shares_us: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  shares_private: { profile: "shares", broker: "Carta" },
  brokerage: { profile: "brokerage", broker: "Sharesies" },
  crypto: { profile: "crypto", subtype: "wallet", tax_treatment: "taxable" },
};

/** The kinds whose account-level institution is required. */
const INSTITUTION_REQUIRED: AccountKind[] = ["bank", "savings", "credit_card", "revolving_credit"];

/**
 * Create an account, asserting success, and return it.
 *
 * Saving an account means answering for its kind's identifying fields — and, on create, its
 * opening balance. Rather than have every spec restate a lender or broker it doesn't care
 * about, this fills in the required set per kind ({@link REQUIRED_METADATA}) underneath
 * whatever the caller passes, so a spec asserting on metadata still gets exactly what it
 * asked for. The opening balance defaults to zero, which deliberately seeds no ledger row,
 * leaving each spec's own transactions/valuations the only ones present.
 *
 * `ownership` is required by the API — every account belongs to a household member or is
 * joint. Specs that aren't about attribution get `joint`, which needs no person to exist;
 * the ones that are pass their own.
 */
export async function createAccount(
  api: SureClient,
  name: string,
  kind: AccountKind,
  currency = "NZD",
  extra: {
    metadata?: Schemas["AccountMetadata"];
    institution?: string;
    ownership?: Schemas["Ownership"];
    opening_balance_minor?: number;
    opening_balance_date?: string;
  } = {}
) {
  const { metadata, institution, ownership, ...openingBalance } = extra;
  // A brokerage account is the one kind with no opening balance: its value comes from the
  // holdings ledger.
  const openingBalanceDefaults =
    kind === "brokerage" ? {} : { opening_balance_minor: 0, opening_balance_date: "2020-01-01" };
  const { data, response } = await api.POST("/api/accounts", {
    body: {
      name,
      kind,
      currency_code: currency,
      archived: false,
      sort_order: 0,
      institution: institution ?? (INSTITUTION_REQUIRED.includes(kind) ? "ANZ" : undefined),
      ownership: ownership ?? { kind: "joint" },
      metadata: withRequiredMetadata(kind, metadata),
      ...openingBalanceDefaults,
      ...openingBalance,
    },
  });
  expect(response.status, "create account").toBe(201);
  return data!;
}

function withRequiredMetadata(kind: AccountKind, provided?: Schemas["AccountMetadata"]) {
  const required = REQUIRED_METADATA[kind];
  if (!required) return provided;
  const merged: Record<string, unknown> = { ...required, ...provided };
  // `address` is the legacy alias of `address_line1` — the same serde field, so a spec
  // writing the old key must not also be handed the new one (serde rejects both at once).
  if (provided && "address" in provided) delete merged.address_line1;
  return merged as Schemas["AccountMetadata"];
}

export async function createCategory(
  api: SureClient,
  name: string,
  kind: CategoryKind = "expense",
  parentId: number | null = null
) {
  const { data, response } = await api.POST("/api/categories", {
    body: { name, kind, parent_id: parentId, sort_order: 0 },
  });
  expect(response.status, "create category").toBe(201);
  return data!;
}

export async function createMerchant(api: SureClient, name: string, categoryId: number | null = null) {
  const { data, response } = await api.POST("/api/merchants", {
    body: { name, category_id: categoryId },
  });
  expect(response.status, "create merchant").toBe(201);
  return data!;
}

export async function createTransaction(
  api: SureClient,
  input: {
    account_id: number;
    posted_at: string;
    amount_minor: number;
    description?: string;
    category_id?: number | null;
    merchant_id?: number | null;
    is_one_off?: boolean;
    /** Attribution override; omitted means "follow the account's owner", as an import does. */
    ownership?: Schemas["Ownership"];
  }
) {
  const { data, response } = await api.POST("/api/transactions", {
    body: {
      account_id: input.account_id,
      posted_at: input.posted_at,
      amount_minor: input.amount_minor,
      description: input.description ?? "x",
      category_id: input.category_id ?? null,
      merchant_id: input.merchant_id ?? null,
      is_one_off: input.is_one_off ?? false,
      ownership: input.ownership ?? null,
    },
  });
  expect(response.status, "create transaction").toBe(201);
  return data!;
}

export async function getTransaction(api: SureClient, id: number) {
  const { data, response } = await api.GET("/api/transactions/{id}", {
    params: { path: { id } },
  });
  expect(response.status).toBe(200);
  return data!;
}

// ---- a tiny zip builder (no dependency; the Rust `zip` reader accepts it) ---------------
//
// STORE by default. `{ deflate: true }` switches to method 8, which a test needs to build
// something small on the wire that expands enormously — a stored entry can't do that.

export function crc32(buf: Buffer): number {
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    let c = (crc ^ buf[i]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

export function makeZip(
  files: Record<string, string | Uint8Array>,
  opts: { deflate?: boolean } = {}
): ArrayBuffer {
  const method = opts.deflate ? 8 : 0;
  const localParts: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, content] of Object.entries(files)) {
    const nameBuf = Buffer.from(name, "utf8");
    const data = typeof content === "string" ? Buffer.from(content, "utf8") : Buffer.from(content);
    // The CRC and the "uncompressed size" field both describe the original bytes; only the
    // stored payload and its size change when deflating.
    const crc = crc32(data);
    const payload = opts.deflate ? deflateRawSync(data) : data;

    const local = Buffer.alloc(30 + nameBuf.length);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(method, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(payload.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    nameBuf.copy(local, 30);
    localParts.push(local, payload);

    const cd = Buffer.alloc(46 + nameBuf.length);
    cd.writeUInt32LE(0x02014b50, 0);
    cd.writeUInt16LE(20, 4);
    cd.writeUInt16LE(20, 6);
    cd.writeUInt16LE(method, 10);
    cd.writeUInt32LE(crc, 16);
    cd.writeUInt32LE(payload.length, 20);
    cd.writeUInt32LE(data.length, 24);
    cd.writeUInt16LE(nameBuf.length, 28);
    cd.writeUInt32LE(offset, 42);
    nameBuf.copy(cd, 46);
    central.push(cd);

    offset += local.length + payload.length;
  }
  const cdBuf = Buffer.concat(central);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(central.length, 8);
  eocd.writeUInt16LE(central.length, 10);
  eocd.writeUInt32LE(cdBuf.length, 12);
  eocd.writeUInt32LE(offset, 16);
  const full = Buffer.concat([...localParts, cdBuf, eocd]);
  // Copy into a freshly-allocated ArrayBuffer so the type is a plain `ArrayBuffer` (a
  // valid `BodyInit`) rather than Node's `Buffer<ArrayBufferLike>`, which TS rejects.
  const ab = new ArrayBuffer(full.byteLength);
  new Uint8Array(ab).set(full);
  return ab;
}

// ---- oversized bodies -------------------------------------------------------------------

/** Which endpoint an oversized-body probe should hit. */
export type OversizedTarget = { path?: string; contentType?: string };

/**
 * One attempt at POSTing `bytes` of body, resolving with the server's answer — or `null`
 * if the connection died before a complete response could be read.
 *
 * Deliberately not `fetch`. The body cap is enforced part-way through the upload: the
 * server reads up to the limit, answers 413, and closes with the rest of the body still
 * unread in its receive buffer — which makes the close an RST rather than a FIN, and an
 * RST tells the client's kernel to discard whatever it has buffered, response included.
 * `undici` turns that into `TypeError: fetch failed` and loses the 413 it had already
 * been sent. Reading the socket directly wins that race nearly every time, but "nearly"
 * is why [`postOversized`] retries: the outcome is decided in the kernel, not here, which
 * is why this only ever failed under the load of a full suite run.
 */
function attemptOversized(
  baseURL: string,
  bytes: number,
  path: string,
  contentType: string,
): Promise<Response | null> {
  const url = new URL(baseURL);
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: url.hostname, port: Number(url.port) });
    const chunks: Buffer[] = [];
    // The refusal closes the connection under our feet; that is the expected ending here,
    // not a failure. A response that never arrives is caught by the test timeout.
    socket.on("error", () => {});
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("close", () => {
      const raw = Buffer.concat(chunks).toString("latin1");
      const separator = raw.indexOf("\r\n\r\n");
      // Reset before the response could be read in full — the caller tries again.
      if (separator === -1) return resolve(null);
      const [statusLine, ...headerLines] = raw.slice(0, separator).split("\r\n");
      const headers = new Headers(
        headerLines.map((line) => {
          const at = line.indexOf(":");
          return [line.slice(0, at), line.slice(at + 1).trim()] as [string, string];
        }),
      );
      // Rebuilt as a `Response` so the assertions below read like every other test here.
      resolve(
        new Response(raw.slice(separator + 4), {
          status: Number(statusLine.split(" ")[1]),
          headers,
        }),
      );
    });
    socket.on("connect", () => {
      socket.write(
        `POST ${path} HTTP/1.1\r\nHost: ${url.host}\r\n` +
          `Content-Type: ${contentType}\r\nContent-Length: ${bytes}\r\n\r\n`,
      );
      // Written in chunks, and only while the socket is still up: one 3 MB `write` would
      // sit in Node's buffer and keep the process alive after the peer had gone.
      const chunk = Buffer.alloc(64 * 1024, "a");
      let sent = 0;
      const pump = () => {
        while (sent < bytes && !socket.destroyed && !socket.writableEnded) {
          const size = Math.min(chunk.length, bytes - sent);
          sent += size;
          if (!socket.write(chunk.subarray(0, size))) return socket.once("drain", pump);
        }
      };
      pump();
    });
  });
}

/**
 * POST an oversized body and return the server's refusal, retrying past a connection that
 * was reset before the response could be read (see [`attemptOversized`]).
 *
 * Retrying does not weaken what is under test: only the guard produces a response at all,
 * so a server that stopped refusing oversized bodies fails every attempt on the status
 * assertion rather than being retried into passing.
 */
export async function postOversized(
  baseURL: string,
  bytes: number,
  { path = "/api/accounts", contentType = "application/json" }: OversizedTarget = {},
): Promise<Response> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const res = await attemptOversized(baseURL, bytes, path, contentType);
    if (res) return res;
  }
  throw new Error("the server never returned a complete response to an oversized body");
}
