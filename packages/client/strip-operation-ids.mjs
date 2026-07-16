// utoipa derives each operationId from its Rust handler's function name. Because
// handlers across modules share generic names (list, create, get_one, delete, …),
// the generated spec has duplicate operationIds. openapi-typescript keys its
// `operations` map by operationId, so duplicates collapse into one (wrong) type.
// Removing operationIds makes it emit per-path inline types instead — which is what
// openapi-fetch consumes — giving each endpoint its correct request/response types.
import { readFileSync, writeFileSync } from "node:fs";

const file = new URL("./openapi.json", import.meta.url);
const spec = JSON.parse(readFileSync(file, "utf8"));
let stripped = 0;
for (const methods of Object.values(spec.paths ?? {})) {
  for (const op of Object.values(methods)) {
    if (op && typeof op === "object" && "operationId" in op) {
      delete op.operationId;
      stripped++;
    }
  }
}
writeFileSync(file, JSON.stringify(spec, null, 2));
console.log(`stripped ${stripped} operationIds`);
