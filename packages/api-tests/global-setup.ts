import { execSync } from "node:child_process";
import path from "node:path";

// Build the backend once so every test can spawn the compiled binary quickly.
export default async function globalSetup() {
  const repoRoot = path.resolve(process.cwd(), "..", "..");
  execSync("cargo build -p sure-api", { cwd: repoRoot, stdio: "inherit" });
}
