import path from "node:path";

import { assertNothingUnstubbed, stopRecordedProxy } from "./global-setup";
import { stopRecordedServer } from "./server-lifecycle";

export default async function globalTeardown() {
  // The backend first. It is the only thing talking to the proxy, and stopping the proxy from
  // under an in-flight provider request would answer a real question — "did the app reach an
  // upstream?" — with a connection error instead of the 503 that names it.
  stopRecordedServer(path.join(process.cwd(), "tests", ".server.pid"));
  // Imported from global-setup because that is where the process is held; Playwright runs both
  // hooks in its main process, so the handle is right there and the pid file stays a fallback.
  await stopRecordedProxy();
  // Last, and after both processes are down: a run that reached an upstream nobody stubbed is a
  // failed run, not a WARN in the scrollback. Thrown here rather than per-test because this suite
  // shares one proxy across the whole run and never touches its control plane, so "which test did
  // it" is not a question this side can answer — the URI in the message is.
  assertNothingUnstubbed();
}
