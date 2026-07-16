// Unit check for the rule builder's emit/parse (`src/lib/rules/expr.ts`): asserts
// tree -> Zen -> tree round-trips and the exact seed expressions. Run: `pnpm test:expr`.
// esbuild transpiles the TS on the fly; it's a transitive dep (via vite) so it isn't
// symlinked at a resolvable node_modules root — locate it in the pnpm store instead.
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { mkdtempSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const require = createRequire(import.meta.url);
const pnpmDir = path.resolve(process.cwd(), "../../node_modules/.pnpm");
const esbuildPkg = readdirSync(pnpmDir).find((d) => d.startsWith("esbuild@"));
if (!esbuildPkg) throw new Error("esbuild not found in pnpm store — run `pnpm install`");
const { build } = require(path.join(pnpmDir, esbuildPkg, "node_modules/esbuild/lib/main.js"));

const out = path.join(mkdtempSync(path.join(tmpdir(), "expr-")), "expr.mjs");
await build({
  entryPoints: [path.resolve("src/lib/rules/expr.ts")],
  bundle: true,
  format: "esm",
  outfile: out,
  logLevel: "warning",
});
const E = await import(pathToFileURL(out).href);

let fails = 0;
const eq = (a, b, msg) => {
  if (a !== b) {
    fails++;
    console.error(`FAIL ${msg}\n  expected: ${JSON.stringify(b)}\n  actual:   ${JSON.stringify(a)}`);
  } else console.log(`ok   ${msg}`);
};

const refs = {
  categories: [{ id: 3, name: "Groceries" }, { id: 7, name: "Dining" }],
  merchants: [{ id: 5, name: "Netflix" }],
  accounts: [{ id: 1, name: "Everyday" }, { id: 2, name: "Visa" }],
  accountKinds: ["bank", "credit_card"],
  currencies: ["NZD", "USD"],
};

// A tree -> Zen -> tree round-trip: the re-emitted Zen must be identical.
function roundtrip(build, label) {
  const zen = E.emit(build());
  const reparsed = E.parse(zen);
  const zen2 = reparsed ? E.emit(reparsed) : "<null>";
  eq(zen2, zen, `roundtrip: ${label}  (${zen})`);
}

const C = (field, op, values = []) => ({ kind: "condition", id: E.uid(), field, op, values });
const G = (combinator, children) => ({ kind: "group", id: E.uid(), combinator, children });

// Seed rule 1 (exact string the seeder writes).
const seed1 =
  "is_expense and (contains(lower(description), 'countdown') or contains(lower(description), 'new world') or contains(lower(description), \"pak'nsave\"))";
const p1 = E.parse(seed1);
eq(E.emit(p1), seed1, "seed 1 parses & re-emits identically");
eq(E.humanize(p1, refs), "Expense and Description contains countdown, new world, pak'nsave", "seed 1 humanizes");

// Seed rule 2.
const seed2 = "contains(lower(description), 'netflix')";
eq(E.emit(E.parse(seed2)), seed2, "seed 2 parses & re-emits identically");

// Emit shapes.
eq(E.emit(G("and", [C("amount", "gt", ["100"])])), "abs_amount > 100", "money gt");
eq(E.emit(G("and", [C("amount", "between", ["10", "20"])])), "abs_amount in [10..20]", "money between");
eq(E.emit(G("and", [C("category", "any_of", ["3", "7"])])), "category_id in [3, 7]", "category any_of");
eq(E.emit(G("and", [C("category", "none_of", ["3"])])), "not (category_id in [3])", "category none_of");
eq(E.emit(G("and", [C("category", "is_set", [])])), "category_id != null", "category is_set");
eq(E.emit(G("and", [C("account_kind", "any_of", ["bank", "credit_card"])])), "account_kind in ['bank', 'credit_card']", "account_kind any_of");
eq(E.emit(G("and", [C("month", "any_of", ["12"])])), "month in [12]", "month any_of numeric");
eq(E.emit(G("and", [C("is_one_off", "is_true", [])])), "is_one_off", "bool is_true");
eq(E.emit(G("and", [C("is_one_off", "is_false", [])])), "not is_one_off", "bool is_false");
eq(E.emit(G("and", [C("description", "equals", ["Rent"])])), "lower(description) in ['rent']", "text equals lowercases");
eq(E.emit(G("and", [C("description", "not_equals", ["a", "b"])])), "not (lower(description) in ['a', 'b'])", "text not_equals");
eq(E.emit(G("and", [C("description", "empty", [])])), "description == ''", "text empty");
eq(E.emit(G("and", [C("merchant", "starts_with", ["Uber"])])), "startsWith(lower(merchant), 'uber')", "text starts_with");
eq(E.emit(G("and", [C("notes", "regex", ["^INV-[0-9]+"])])), "matches(notes, '^INV-[0-9]+')", "regex keeps case");

// Nested groups: A and (B or C).
const nested = G("and", [
  C("direction", "is_expense", []),
  G("or", [C("amount", "gt", ["500"]), C("account", "any_of", ["2"])]),
]);
eq(E.emit(nested), "is_expense and (abs_amount > 500 or account_id in [2])", "nested and(or)");

// Round-trips over a spread of shapes.
roundtrip(() => nested, "nested and(or)");
roundtrip(() => G("or", [C("amount", "between", ["0", "50"]), C("currency", "none_of", ["USD"])]), "or with between + none_of");
roundtrip(() => G("and", [C("description", "not_contains", ["fee"]), C("category", "not_set", [])]), "not_contains + not_set");
roundtrip(() => G("and", [C("description", "contains", ["a", "b", "c"])]), "multi contains collapses");
roundtrip(() => G("and", [C("month", "none_of", ["1", "2"]), C("year", "gte", ["2024"])]), "month none_of + year");

// Unparseable expressions fall back to null.
eq(E.parse("some_unknown_fn(x) > 3"), null, "unknown → null");
eq(E.parse("amount < -100"), null, "signed amount (not a builder field) → null");
eq(E.parse("this is not (valid"), null, "garbage → null");

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILED`);
process.exit(fails === 0 ? 0 : 1);
