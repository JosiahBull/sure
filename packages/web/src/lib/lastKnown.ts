/**
 * The last answer this device saw for the reads a launch draws first, kept so the next launch
 * can draw them before the network has answered.
 *
 * # Why
 *
 * The shell of the installed app is already instant: `index.html`, the bundle, the stylesheet
 * and the fonts come out of the service worker's precache. Everything after it was not. A launch
 * after iOS has suspended the app starts with no open connection, so the first API request pays
 * a TCP and a TLS handshake before it can be sent — three round trips before a single figure,
 * about a second from the UK to a server in New Zealand however fast the server is (the reports
 * a launch waits on take single-digit milliseconds to compute). Until then every card was a
 * spinner.
 *
 * # The guarantee this keeps
 *
 * `packages/api/src/cache.rs` gives reports no `max-age` because stale numbers right after
 * editing a transaction are worse than slowness, and nothing here may undo that from the client.
 * So this is not an HTTP cache and never stands in for the network:
 *
 * - **Every read still goes to the server.** A kept value is drawn while the request is in
 *   flight, marked stale, and replaced by whatever the server answers. Nothing here can satisfy
 *   a request, so the service worker still leaves `/api` alone and the server's headers are
 *   unchanged.
 * - **Any mutation forgets everything.** Every write goes through {@link mutating} — the shared
 *   client's write verbs are wrapped in it (`api.ts`), and the one raw `fetch` upload calls it
 *   itself — which clears the store before the request is sent, so the read that follows an
 *   edit has nothing to draw but a spinner and then the server's answer. Forgetting per
 *   endpoint would mean knowing which reports a valuation feeds; forgetting all of it costs one
 *   launch without a head start.
 * - **A read that overlapped a mutation is not kept.** Its answer may predate the write, and
 *   keeping it would put the pre-edit figure back on the next launch.
 *
 * Edits made somewhere else — another device, a bank sync on the server — are not visible here
 * until the refresh lands, which is the case the stale marking exists for.
 *
 * # What is kept, and the bounds
 *
 * Only what a call site asks for by name, which today is what the launch screen draws: the
 * base currency, the account balances (now and at the start of the period) and the dashboard's
 * net-worth series and money-flow graph, per preset period. Summaries, sized by accounts and
 * categories rather than by transactions; never a transaction list. It lives in `localStorage`
 * rather than IndexedDB because the read has to be synchronous to be drawn in the first frame,
 * and it is bounded three ways:
 *
 * - **By shape.** The store is stamped with a hash of the API's generated types and this
 *   module's own {@link FORMAT} ({@link VERSION}); a store with any other stamp is discarded
 *   unread, so an old shape is never drawn by code that expects a new one. Deliberately *not*
 *   the app version: `pwa.ts` reloads a running app into a new build at the next change of page
 *   or return to the foreground, and a release that changed no response shape should land on
 *   the figures it left rather than on a spinner.
 * - **By size.** {@link MAX_ENTRIES} entries and {@link MAX_CHARS} characters of JSON in all,
 *   oldest evicted first; an entry over {@link MAX_ENTRY_CHARS} is not kept at all.
 * - **By age.** An entry older than {@link MAX_AGE_MS} is dropped rather than drawn.
 *
 * It is the household's own data on the household's own device, which the server's
 * `private` already allows; nothing here changes what a shared cache may hold.
 */

/**
 * What a kept entry is, beyond the API response it holds. Bump it when that changes — an entry
 * stored as something other than the raw response, say — since the API types alone will not
 * notice.
 */
const FORMAT = 1;

/** The stamp a store must carry to be read: {@link FORMAT} and a hash of the API's types. */
const VERSION = `${FORMAT}+${__API_SCHEMA_HASH__}`;

const STORAGE_KEY = "sure:last-known";
// Sized against what is kept on a ledger of 8,000 transactions: a launch keeps five entries and
// about 30k characters, and every preset visited in one session comes to 25 entries and about
// 240k. Eviction is a backstop, not something a household should ever see.
const MAX_ENTRIES = 32;
const MAX_CHARS = 384 * 1024;
const MAX_ENTRY_CHARS = 96 * 1024;
const MAX_AGE_MS = 30 * 86_400_000;

/** A kept answer and when the server gave it. */
export type Kept<T> = { data: T; at: number };

type Store = { v: string; entries: Record<string, Kept<unknown>> };

/** The stored entries if they carry this {@link VERSION}, `null` if not or if there are none. */
function read(raw: string | null): Record<string, Kept<unknown>> | null {
  if (!raw) return null;
  try {
    const store = JSON.parse(raw) as Partial<Store>;
    if (store.v !== VERSION || typeof store.entries !== "object" || store.entries === null) {
      return null;
    }
    return store.entries;
  } catch {
    return null;
  }
}

/** The parsed store, read once at startup; every write goes to both. */
let memory: Record<string, Kept<unknown>> = load();

function load(): Record<string, Kept<unknown>> {
  try {
    const entries = read(localStorage.getItem(STORAGE_KEY));
    // Anything else under the key is another shape's, and this build is the one writing now.
    if (!entries) localStorage.removeItem(STORAGE_KEY);
    return entries ?? {};
  } catch {
    // Storage is unavailable (a private window), so there is nothing to draw — which is the
    // behaviour without this module.
    return {};
  }
}

function persist(): void {
  try {
    if (Object.keys(memory).length === 0) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, JSON.stringify({ v: VERSION, entries: memory }));
  } catch {
    // Over quota. Better to keep nothing than to keep failing on every read.
    memory = {};
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      /* storage is unavailable */
    }
  }
}

/**
 * The answer last kept under `key`, or `null` when there is none of this shape. A `null` key
 * is a read that is not kept (a brushed period), so it never has one.
 */
export function lastKnown<T>(key: string | null): Kept<T> | null {
  if (key === null) return null;
  const hit = memory[key];
  if (!hit) return null;
  if (Date.now() - hit.at > MAX_AGE_MS) {
    delete memory[key];
    persist();
    return null;
  }
  return hit as Kept<T>;
}

/** Bumped by every mutation's start and settle, so a read can tell whether one overlapped it. */
let generation = 0;
let mutationsInFlight = 0;
const settledListeners = new Set<() => void>();

/**
 * Run a read, and keep its answer under `key` for the next launch.
 *
 * The read's own result is returned untouched; keeping is a side effect. An error is never
 * kept, and neither is an answer from a read that a mutation overlapped — see the module docs.
 */
export async function keep<R extends { data?: unknown; error?: unknown }>(
  key: string | null,
  read: () => Promise<R>,
): Promise<R> {
  const startedAt = generation;
  const result = await read();
  if (
    key !== null &&
    result.error === undefined &&
    result.data !== undefined &&
    startedAt === generation &&
    mutationsInFlight === 0
  ) {
    remember(key, result.data);
  }
  return result;
}

function remember(key: string, data: unknown): void {
  const entry = { data, at: Date.now() };
  if (JSON.stringify(entry).length > MAX_ENTRY_CHARS) {
    delete memory[key];
    persist();
    return;
  }
  memory[key] = entry;
  // Newest first, then trim from the end until both bounds hold.
  const keys = Object.keys(memory).sort((a, b) => memory[b].at - memory[a].at);
  let chars = 0;
  const kept: Record<string, Kept<unknown>> = {};
  for (const k of keys) {
    const size = JSON.stringify(memory[k]).length + k.length;
    if (Object.keys(kept).length >= MAX_ENTRIES || chars + size > MAX_CHARS) break;
    chars += size;
    kept[k] = memory[k];
  }
  memory = kept;
  persist();
}

/** Forget everything kept, here and in storage. */
function forgetAll(): void {
  memory = {};
  persist();
}

/**
 * Be told whenever a mutation settles — for long-lived state that outlives the page that made
 * the edit (the account panel is the one that matters). Returns the unsubscribe function.
 */
export function onMutationSettled(listener: () => void): () => void {
  settledListeners.add(listener);
  return () => {
    settledListeners.delete(listener);
  };
}

/**
 * Run a mutation, forgetting everything kept before it is sent.
 *
 * The shared client's write verbs all go through this (`api.ts`); the one raw `fetch` upload
 * calls it directly. Settled listeners run whether or not it succeeded — a failed write may
 * still have changed something.
 */
export async function mutating<T>(run: () => Promise<T>): Promise<T> {
  mutationsInFlight++;
  generation++;
  forgetAll();
  try {
    return await run();
  } finally {
    mutationsInFlight--;
    generation++;
    for (const listener of settledListeners) listener();
  }
}

// Another tab's mutation clears the shared store, and a read in flight here must not write a
// pre-edit answer back into it — so a removal counts as a mutation. An ordinary write from that
// tab only needs picking up, and one from a tab still on another build's shape — open until
// `pwa.ts` reloads it — is left alone rather than deleted out from under it.
window.addEventListener("storage", (e) => {
  if (e.key !== STORAGE_KEY && e.key !== null) return;
  if (e.newValue === null) {
    generation++;
    memory = {};
    return;
  }
  memory = read(e.newValue) ?? memory;
});
