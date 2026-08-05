import path from "node:path";

import { stopRecordedProxy } from "./global-setup";
import { stopRecordedServer } from "./server-lifecycle";

export default async function globalTeardown() {
  // The backend first. It is the only thing talking to the proxy, and stopping the proxy from
  // under an in-flight provider request would answer a real question — "did the app reach an
  // upstream?" — with a connection error instead of the 503 that names it.
  stopRecordedServer(path.join(process.cwd(), "tests", ".server.pid"));
  // Imported from global-setup because that is where the process is held; Playwright runs both
  // hooks in its main process, so the handle is right there and the pid file stays a fallback.
  await stopRecordedProxy();
}
