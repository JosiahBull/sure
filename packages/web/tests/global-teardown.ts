import { readFileSync, rmSync } from "node:fs";
import path from "node:path";

export default async function globalTeardown() {
  const pidFile = path.join(process.cwd(), "tests", ".server.pid");
  try {
    const pid = Number(readFileSync(pidFile, "utf8").trim());
    if (pid) {
      // Kill the detached process group.
      try {
        process.kill(-pid, "SIGTERM");
      } catch {
        process.kill(pid, "SIGTERM");
      }
    }
  } catch {
    /* nothing to stop */
  } finally {
    rmSync(pidFile, { force: true });
  }
}
