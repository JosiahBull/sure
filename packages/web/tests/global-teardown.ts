import path from "node:path";

import { stopRecordedServer } from "./server-lifecycle";

export default async function globalTeardown() {
  stopRecordedServer(path.join(process.cwd(), "tests", ".server.pid"));
}
