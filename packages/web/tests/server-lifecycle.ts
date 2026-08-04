import { readFileSync, rmSync } from "node:fs";

/**
 * Starting and stopping the backend the visual suite drives, shared by global-setup and
 * global-teardown so both agree on what "stopped" means.
 *
 * The server is spawned detached (it has to outlive the setup process), and its pid is
 * recorded in a file. Every path that gives up on a run has to go through here, because a
 * server left holding the port is not a failure that stays contained: the next run's
 * readiness probe passes against *it*, the seed then runs against a database that is
 * already seeded, and the suite fails with `POST /api/people -> 409` — a message that
 * points at the API rather than at the leftover process.
 */

/**
 * SIGTERM a server we recorded earlier, and forget the pid either way.
 *
 * Returns whether a signal was actually delivered — the caller waits for the port only when
 * there was something to wait for, so a port held by a process we never started is reported
 * immediately instead of after a pointless drain poll.
 */
export function stopRecordedServer(pidFile: string): boolean {
  try {
    const pid = Number(readFileSync(pidFile, "utf8").trim());
    if (!pid) return false;
    // The process group, because the server is detached and may have children.
    try {
      process.kill(-pid, "SIGTERM");
    } catch {
      process.kill(pid, "SIGTERM");
    }
    return true;
  } catch {
    /* no pid recorded, or it is already gone */
    return false;
  } finally {
    rmSync(pidFile, { force: true });
  }
}

/** Whether anything at all is answering on the suite's port. */
export async function serverResponds(base: string): Promise<boolean> {
  try {
    await fetch(`${base}/api/health`);
    return true;
  } catch {
    return false;
  }
}
