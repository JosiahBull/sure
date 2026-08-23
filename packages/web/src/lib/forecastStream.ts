// Reading `GET /api/forecast/stream`.
//
// Hand-written rather than through `@sure/client`, because `openapi-fetch` resolves a whole
// body — it has nowhere to put an answer that arrives in pieces. The payload type is still
// generated, so the wire shape stays checked against the Rust `ToSchema`.
//
// And deliberately `fetch` + `ReadableStream` rather than `EventSource`, which is the obvious
// tool and the wrong one twice over:
//
//   * it exposes neither the status nor the body of a non-200. This route legitimately answers
//     `503 {"error":{"code":"overloaded"}}` when every compute slot is busy, and `400` for an
//     unknown `?currency=`; through `EventSource` both are an untyped `error` event and the
//     message is gone.
//   * it *reconnects by itself*. Here that means silently restarting a 2 000-path simulation
//     the user has already navigated away from — the exact cost this endpoint exists to avoid.
//
// `AbortSignal` gives the third thing neither of those does: dropping the reader closes the
// response body, the server's channel send fails, and the simulation stops claiming paths
// within a path or two. Superseding a forecast actually cancels it.

import type { Schemas } from "./api";

export type ForecastProgress = Schemas["ForecastProgress"];

/** Which kind of event a payload arrived on. A `tick` never carries a `result`. */
export type ForecastEventKind = "tick" | "snapshot";

export interface ForecastStreamQuery {
  horizon_months: number;
  /** Omitted by the page; here because the api-tests and any hand-driving want it. */
  simulations?: number;
  currency?: string;
  seed?: number;
}

/** The error envelope every `/api` failure uses. */
interface ErrorEnvelope {
  error?: { code?: string; message?: string };
}

/**
 * Stream a forecast, calling `on` for every event until the run finishes.
 *
 * Resolves once the server has sent its terminal `done`. Rejects if the stream ends without one
 * — a truncated stream must not leave a half-run projection on screen labelled as final — or if
 * the response was not a 200, carrying the envelope's own message so "busy, come back" reads as
 * itself. An `AbortError` from `signal` is re-thrown for the caller to recognise and ignore.
 */
export async function streamForecast(
  query: ForecastStreamQuery,
  signal: AbortSignal,
  on: (progress: ForecastProgress, kind: ForecastEventKind) => void
): Promise<void> {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(query)) {
    if (v != null) params.set(k, String(v));
  }
  const res = await fetch(`/api/forecast/stream?${params}`, {
    signal,
    headers: { Accept: "text/event-stream" },
  });

  if (!res.ok) {
    // The body is JSON here, not an event stream: the head is produced before anything is
    // simulated, so a refusal is an ordinary error response.
    let message = `The forecast failed (${res.status}).`;
    try {
      const body = (await res.json()) as ErrorEnvelope;
      if (body.error?.message) message = body.error.message;
    } catch {
      // Not JSON after all. The status-derived message above is what we have.
    }
    throw new Error(message);
  }
  if (!res.body) throw new Error("The forecast stream carried no body.");

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffered = "";
  let terminated = false;

  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buffered += decoder.decode(value, { stream: true });

      // SSE frames are separated by a blank line. Anything after the last one is a partial
      // frame — a 230 KiB snapshot arrives across many chunks — so it stays in the buffer.
      let boundary: number;
      while ((boundary = buffered.indexOf("\n\n")) !== -1) {
        const frame = buffered.slice(0, boundary);
        buffered = buffered.slice(boundary + 2);
        const parsed = parseFrame(frame);
        if (!parsed) continue;
        const { event, data } = parsed;
        if (event === "done") {
          terminated = true;
        } else if (event === "error") {
          let message = "The forecast simulation failed.";
          try {
            const body = JSON.parse(data) as ErrorEnvelope;
            if (body.error?.message) message = body.error.message;
          } catch {
            // Keep the default.
          }
          throw new Error(message);
        } else if (event === "snapshot" || event === "tick") {
          on(JSON.parse(data) as ForecastProgress, event);
        }
        // An unrecognised event name is ignored rather than fatal: the server may grow one.
      }
    }
  } finally {
    // Releasing the lock lets the body be cancelled promptly on the abort path.
    reader.releaseLock();
  }

  if (!terminated) {
    throw new Error("The forecast stream ended before the simulation finished.");
  }
}

/** One SSE frame's `event:` name and joined `data:` payload, or `null` if it carried no data. */
function parseFrame(frame: string): { event: string; data: string } | null {
  let event = "message";
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    // A leading colon is a comment — which is exactly what axum's keep-alive sends.
    if (line === "" || line.startsWith(":")) continue;
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    // One optional space after the colon, per the spec.
    let value = colon === -1 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "event") event = value;
    else if (field === "data") data.push(value);
  }
  return data.length ? { event, data: data.join("\n") } : null;
}
