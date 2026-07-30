// Block until the backend answers, so `pnpm dev` starts Vite only once its `/api` proxy has
// something to talk to. Vite is ready in under a second and the backend is a `cargo run`
// away — minutes on a cold build — so without this the SPA loads first and its opening
// requests fill the log with proxy ECONNREFUSED errors that mean nothing.
//
// Usage: node scripts/wait-for-backend.mjs && <start the frontend>
const ORIGIN = process.env.DEV_API_ORIGIN ?? "http://127.0.0.1:8080";
const HEALTH = `${ORIGIN}/api/health`;
// Generous: the first build of the workspace compiles several hundred crates. This is a
// backstop against waiting forever on a backend that is never coming, not a build budget.
const TIMEOUT_MS = Number(process.env.DEV_API_WAIT_MS ?? 600_000);
const POLL_MS = 250;
// cargo prints its own progress, so say nothing unless the wait lasts long enough that a
// silent frontend looks like a hang.
const ANNOUNCE_AFTER_MS = 2_000;

const started = Date.now();
const elapsed = () => Date.now() - started;
let announced = false;

while (elapsed() < TIMEOUT_MS) {
  try {
    // A request that outlives one poll interval means something is listening but wedged;
    // time it out and try again rather than stacking up connections.
    const res = await fetch(HEALTH, { signal: AbortSignal.timeout(2_000) });
    if (res.ok) {
      if (announced) {
        console.log(`backend is up after ${(elapsed() / 1000).toFixed(1)}s — starting the frontend`);
      }
      process.exit(0);
    }
  } catch {
    // Not listening yet, or still binding. Either way: wait.
  }
  if (!announced && elapsed() > ANNOUNCE_AFTER_MS) {
    announced = true;
    console.log(`waiting for the backend on ${ORIGIN} (cargo is probably still building)…`);
  }
  await new Promise((resolve) => setTimeout(resolve, POLL_MS));
}

console.error(
  `backend did not answer ${HEALTH} within ${(TIMEOUT_MS / 1000).toFixed(0)}s — not starting the frontend. ` +
    `Check the api output above; DEV_API_WAIT_MS raises the limit, DEV_API_ORIGIN changes the address.`,
);
process.exit(1);
