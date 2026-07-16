// Visual rule builder ⇄ Zen expression.
//
// The backend evaluates a Zen expression string against a transaction context (see
// `sure_dal::rules` / the API's rules route). This module is the single source of
// truth for turning a friendly AND/OR condition tree into that string (`emit`) and
// for reconstructing the tree from a string when editing (`parse`). `parse` only
// understands the subset `emit` produces (plus the shapes the seed uses); anything
// it can't recognise returns `null`, and the UI falls back to a raw-expression editor
// — so hand-written expressions still work, they just aren't shown in the builder.
//
// Zen quirks that shape the code below (verified against zen-expression 0.55):
//   • String literals have NO escape sequences — a string runs to the next matching
//     quote. So to embed a `'` we switch to double quotes (see `zenStr`), matching how
//     the seed writes "pak'nsave".
//   • `x in ['a','b']` is list membership; `x in [a..b]` is an inclusive interval.
//   • Case-insensitive text matching wraps the field in `lower(...)`, so the literal is
//     lower-cased too.

export type Combinator = "and" | "or";

export interface Group {
  kind: "group";
  id: number;
  combinator: Combinator;
  children: RuleNode[];
}

export interface Condition {
  kind: "condition";
  id: number;
  field: string; // FieldDef.key
  op: string; // OpDef.key
  values: string[]; // stringified; interpreted per field type on emit
}

export type RuleNode = Group | Condition;

// ---- fields ----------------------------------------------------------------

export type FieldType = "text" | "money" | "int" | "bool" | "direction" | "enum" | "ref";

export interface FieldDef {
  key: string;
  label: string;
  type: FieldType;
  /** Identifier used in the Zen context. Empty for the synthetic `direction` field. */
  zen: string;
  ref?: "category" | "merchant" | "account";
  enumSource?: "account_kind" | "currency" | "month";
  unit?: string;
  placeholder?: string;
}

export const FIELDS: FieldDef[] = [
  { key: "description", label: "Description", type: "text", zen: "description", placeholder: "e.g. countdown" },
  { key: "merchant", label: "Merchant name", type: "text", zen: "merchant", placeholder: "e.g. netflix" },
  { key: "amount", label: "Amount", type: "money", zen: "abs_amount", unit: "$" },
  { key: "direction", label: "Direction", type: "direction", zen: "" },
  { key: "category", label: "Category", type: "ref", zen: "category_id", ref: "category" },
  { key: "account", label: "Account", type: "ref", zen: "account_id", ref: "account" },
  { key: "account_kind", label: "Account type", type: "enum", zen: "account_kind", enumSource: "account_kind" },
  { key: "assigned_merchant", label: "Assigned merchant", type: "ref", zen: "merchant_id", ref: "merchant" },
  { key: "currency", label: "Currency", type: "enum", zen: "currency", enumSource: "currency" },
  { key: "notes", label: "Notes", type: "text", zen: "notes", placeholder: "e.g. reimburse" },
  { key: "is_one_off", label: "One-off", type: "bool", zen: "is_one_off" },
  { key: "year", label: "Year", type: "int", zen: "year", placeholder: "2026" },
  { key: "month", label: "Month", type: "enum", zen: "month", enumSource: "month" },
  { key: "day", label: "Day of month", type: "int", zen: "day", placeholder: "1–31" },
];

const FIELD_BY_KEY = new Map(FIELDS.map((f) => [f.key, f]));
export const fieldDef = (key: string): FieldDef | undefined => FIELD_BY_KEY.get(key);
const fieldByZen = (zen: string): FieldDef | undefined =>
  zen ? FIELDS.find((f) => f.zen === zen) : undefined;

// ---- operators -------------------------------------------------------------

export type Arity = "none" | "one" | "two" | "many";
export interface OpDef {
  key: string;
  label: string;
  arity: Arity;
}

const NUMERIC_OPS: OpDef[] = [
  { key: "eq", label: "is exactly", arity: "one" },
  { key: "neq", label: "is not", arity: "one" },
  { key: "gt", label: "is greater than", arity: "one" },
  { key: "gte", label: "is at least", arity: "one" },
  { key: "lt", label: "is less than", arity: "one" },
  { key: "lte", label: "is at most", arity: "one" },
  { key: "between", label: "is between", arity: "two" },
];

const OPS: Record<FieldType, OpDef[]> = {
  text: [
    { key: "contains", label: "contains", arity: "many" },
    { key: "not_contains", label: "does not contain", arity: "one" },
    { key: "equals", label: "is any of", arity: "many" },
    { key: "not_equals", label: "is none of", arity: "many" },
    { key: "starts_with", label: "starts with", arity: "one" },
    { key: "ends_with", label: "ends with", arity: "one" },
    { key: "regex", label: "matches regex", arity: "one" },
    { key: "empty", label: "is empty", arity: "none" },
    { key: "not_empty", label: "is not empty", arity: "none" },
  ],
  money: NUMERIC_OPS,
  int: NUMERIC_OPS,
  bool: [
    { key: "is_true", label: "is yes", arity: "none" },
    { key: "is_false", label: "is no", arity: "none" },
  ],
  direction: [
    { key: "is_expense", label: "is expense", arity: "none" },
    { key: "is_income", label: "is income", arity: "none" },
  ],
  enum: [
    { key: "any_of", label: "is any of", arity: "many" },
    { key: "none_of", label: "is none of", arity: "many" },
  ],
  ref: [
    { key: "any_of", label: "is any of", arity: "many" },
    { key: "none_of", label: "is none of", arity: "many" },
    { key: "is_set", label: "is set", arity: "none" },
    { key: "not_set", label: "is not set", arity: "none" },
  ],
};

export const opsFor = (type: FieldType): OpDef[] => OPS[type];
export const defaultOp = (type: FieldType): string => OPS[type][0].key;
export const opDef = (type: FieldType, key: string): OpDef | undefined =>
  OPS[type].find((o) => o.key === key);
export const arityOf = (type: FieldType, key: string): Arity => opDef(type, key)?.arity ?? "one";

// ---- choice options (enum + ref fields) ------------------------------------

export interface BuilderRefs {
  categories: { id: number; name: string }[];
  merchants: { id: number; name: string }[];
  accounts: { id: number; name: string }[];
  accountKinds: string[];
  currencies: string[];
}

const MONTHS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

export const kindLabel = (k: string): string =>
  k.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());

export interface Choice {
  value: string;
  label: string;
}

export function choiceOptions(f: FieldDef, refs: BuilderRefs): Choice[] {
  if (f.type === "ref") {
    const list =
      f.ref === "category" ? refs.categories : f.ref === "account" ? refs.accounts : refs.merchants;
    return list.map((x) => ({ value: String(x.id), label: x.name }));
  }
  if (f.enumSource === "account_kind") return refs.accountKinds.map((k) => ({ value: k, label: kindLabel(k) }));
  if (f.enumSource === "currency") return refs.currencies.map((c) => ({ value: c, label: c }));
  if (f.enumSource === "month") return MONTHS.map((m, i) => ({ value: String(i + 1), label: m }));
  return [];
}

export const choiceLabel = (f: FieldDef, value: string, refs: BuilderRefs): string =>
  choiceOptions(f, refs).find((o) => o.value === value)?.label ?? value;

// ---- constructors ----------------------------------------------------------

let _uid = 0;
export const uid = (): number => ++_uid;

export const newCondition = (field = "description"): Condition => ({
  kind: "condition",
  id: uid(),
  field,
  op: defaultOp(fieldDef(field)!.type),
  values: [],
});

export const newGroup = (combinator: Combinator = "and", withChild = true): Group => ({
  kind: "group",
  id: uid(),
  combinator,
  children: withChild ? [newCondition()] : [],
});

export const emptyRoot = (): Group => newGroup("and", true);

// ---- emit (tree -> Zen) ----------------------------------------------------

/** Quote a string as a Zen literal. Zen has no escapes, so switch quote style to
 *  embed the opposite quote (e.g. "pak'nsave"). A value with both quote kinds can't
 *  be represented — `isRepresentable` flags it and the UI warns rather than emit junk. */
export function zenStr(v: string): string {
  if (v.includes("'") && !v.includes('"')) return `"${v}"`;
  return `'${v}'`;
}
export const isRepresentable = (v: string): boolean => !(v.includes("'") && v.includes('"'));

const cleanVals = (vs: string[]): string[] => {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const raw of vs) {
    const v = (raw ?? "").trim();
    if (v && !seen.has(v)) {
      seen.add(v);
      out.push(v);
    }
  }
  return out;
};

const numOrNull = (v: string | undefined): number | null => {
  if (v == null || v.trim() === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
};

/** Join Zen terms with `or`, wrapping in parens when there's more than one. */
const orJoin = (parts: string[]): string =>
  parts.length <= 1 ? (parts[0] ?? "") : `(${parts.join(" or ")})`;

const CMP: Record<string, string> = { eq: "==", neq: "!=", gt: ">", gte: ">=", lt: "<", lte: "<=" };

function emitText(z: string, c: Condition): string {
  const vals = cleanVals(c.values);
  const lc = (v: string) => zenStr(v.toLowerCase());
  switch (c.op) {
    case "contains":
      return orJoin(vals.map((v) => `contains(lower(${z}), ${lc(v)})`));
    case "not_contains":
      return vals[0] ? `not contains(lower(${z}), ${lc(vals[0])})` : "";
    case "equals":
      return vals.length ? `lower(${z}) in [${vals.map(lc).join(", ")}]` : "";
    case "not_equals":
      return vals.length ? `not (lower(${z}) in [${vals.map(lc).join(", ")}])` : "";
    case "starts_with":
      return vals[0] ? `startsWith(lower(${z}), ${lc(vals[0])})` : "";
    case "ends_with":
      return vals[0] ? `endsWith(lower(${z}), ${lc(vals[0])})` : "";
    case "regex":
      return vals[0] ? `matches(${z}, ${zenStr(vals[0])})` : "";
    case "empty":
      return `${z} == ''`;
    case "not_empty":
      return `${z} != ''`;
  }
  return "";
}

function emitNumber(z: string, c: Condition): string {
  if (c.op === "between") {
    const a = numOrNull(c.values[0]);
    const b = numOrNull(c.values[1]);
    return a != null && b != null ? `${z} in [${a}..${b}]` : "";
  }
  const n = numOrNull(c.values[0]);
  return n != null ? `${z} ${CMP[c.op] ?? "=="} ${n}` : "";
}

function emitChoice(z: string, c: Condition, f: FieldDef): string {
  if (c.op === "is_set") return `${z} != null`;
  if (c.op === "not_set") return `${z} == null`;
  const vals = cleanVals(c.values);
  if (!vals.length) return "";
  const numeric = f.type === "ref" || f.enumSource === "month";
  const items = numeric ? vals.map((v) => String(parseInt(v, 10))) : vals.map((v) => zenStr(v));
  const list = `${z} in [${items.join(", ")}]`;
  return c.op === "none_of" ? `not (${list})` : list;
}

function emitCondition(c: Condition): string {
  const f = fieldDef(c.field);
  if (!f) return "";
  switch (f.type) {
    case "text":
      return emitText(f.zen, c);
    case "money":
    case "int":
      return emitNumber(f.zen, c);
    case "direction":
      return c.op === "is_income" ? "is_income" : "is_expense";
    case "bool":
      return c.op === "is_false" ? `not ${f.zen}` : f.zen;
    case "enum":
    case "ref":
      return emitChoice(f.zen, c, f);
  }
}

function emitNode(n: RuleNode): string {
  if (n.kind === "condition") return emitCondition(n);
  const parts = n.children.map(emitNode).filter(Boolean);
  if (parts.length <= 1) return parts[0] ?? "";
  return `(${parts.join(` ${n.combinator} `)})`;
}

/** Turn the root group into a Zen expression. Returns "" when nothing is set. */
export function emit(root: Group): string {
  const parts = root.children.map(emitNode).filter(Boolean);
  return parts.join(` ${root.combinator} `);
}

// ---- humanize (tree -> readable summary) -----------------------------------

function humanizeCondition(c: Condition, refs: BuilderRefs): string {
  const f = fieldDef(c.field);
  if (!f) return "?";
  const op = opDef(f.type, c.op);
  const opLabel = op?.label ?? c.op;
  const vals = cleanVals(c.values);
  if (f.type === "direction") return c.op === "is_income" ? "Income" : "Expense";
  if (f.type === "bool") return `${f.label} ${opLabel}`;
  if (op?.arity === "none") return `${f.label} ${opLabel}`;
  let valueText: string;
  if (f.type === "enum" || f.type === "ref") {
    valueText = vals.map((v) => choiceLabel(f, v, refs)).join(", ");
  } else if (c.op === "between") {
    valueText = `${vals[0] ?? "?"}–${vals[1] ?? "?"}`;
  } else {
    valueText = vals.join(", ");
  }
  const unit = f.unit ?? "";
  return `${f.label} ${opLabel} ${unit}${valueText}`.trim();
}

function humanizeNode(n: RuleNode, refs: BuilderRefs, top = false): string {
  if (n.kind === "condition") return humanizeCondition(n, refs);
  const sep = n.combinator === "and" ? " and " : " or ";
  const parts = n.children.map((c) => humanizeNode(c, refs)).filter(Boolean);
  if (!parts.length) return "";
  if (parts.length === 1) return parts[0];
  const joined = parts.join(sep);
  return top ? joined : `(${joined})`;
}

export const humanize = (root: Group, refs: BuilderRefs): string => humanizeNode(root, refs, true);

// ---- parse (Zen -> tree) ---------------------------------------------------

type Tok =
  | { t: "lparen" | "rparen" | "lbracket" | "rbracket" | "comma" | "range" | "and" | "or" | "not" | "in" | "null" }
  | { t: "bool"; v: boolean }
  | { t: "op"; v: string }
  | { t: "str"; v: string }
  | { t: "num"; v: string }
  | { t: "id"; v: string };

const FUNCS = new Set(["contains", "startsWith", "endsWith", "matches", "lower", "upper"]);

function tokenize(src: string): Tok[] {
  const toks: Tok[] = [];
  let i = 0;
  const n = src.length;
  const isIdStart = (c: string) => /[A-Za-z_]/.test(c);
  const isId = (c: string) => /[A-Za-z0-9_]/.test(c);
  const isDigit = (c: string) => c >= "0" && c <= "9";
  while (i < n) {
    const c = src[i];
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      i++;
      continue;
    }
    if (c === "(") { toks.push({ t: "lparen" }); i++; continue; }
    if (c === ")") { toks.push({ t: "rparen" }); i++; continue; }
    if (c === "[") { toks.push({ t: "lbracket" }); i++; continue; }
    if (c === "]") { toks.push({ t: "rbracket" }); i++; continue; }
    if (c === ",") { toks.push({ t: "comma" }); i++; continue; }
    if (c === "." && src[i + 1] === ".") { toks.push({ t: "range" }); i += 2; continue; }
    if (c === "'" || c === '"') {
      // No escapes: run to the next matching quote (mirrors the Zen lexer).
      const end = src.indexOf(c, i + 1);
      if (end === -1) throw new Error("unterminated string");
      toks.push({ t: "str", v: src.slice(i + 1, end) });
      i = end + 1;
      continue;
    }
    if (c === "=" || c === "!" || c === ">" || c === "<") {
      if (src[i + 1] === "=") { toks.push({ t: "op", v: c + "=" }); i += 2; continue; }
      if (c === ">" || c === "<") { toks.push({ t: "op", v: c }); i++; continue; }
      throw new Error(`unexpected '${c}'`);
    }
    if (isDigit(c) || (c === "-" && isDigit(src[i + 1] ?? ""))) {
      let j = i + 1;
      while (j < n && isDigit(src[j])) j++;
      if (src[j] === "." && src[j + 1] !== ".") {
        j++;
        while (j < n && isDigit(src[j])) j++;
      }
      toks.push({ t: "num", v: src.slice(i, j) });
      i = j;
      continue;
    }
    if (isIdStart(c)) {
      let j = i + 1;
      while (j < n && isId(src[j])) j++;
      const word = src.slice(i, j);
      i = j;
      if (word === "and" || word === "or" || word === "not" || word === "in") toks.push({ t: word });
      else if (word === "null") toks.push({ t: "null" });
      else if (word === "true" || word === "false") toks.push({ t: "bool", v: word === "true" });
      else toks.push({ t: "id", v: word });
      continue;
    }
    throw new Error(`unexpected '${c}'`);
  }
  return toks;
}

class Parser {
  private pos = 0;
  constructor(private toks: Tok[]) {}
  private peek(): Tok | undefined {
    return this.toks[this.pos];
  }
  eof(): boolean {
    return this.pos >= this.toks.length;
  }
  private next(): Tok {
    const t = this.toks[this.pos++];
    if (!t) throw new Error("unexpected end");
    return t;
  }
  private expect(t: Tok["t"]): Tok {
    const tok = this.next();
    if (tok.t !== t) throw new Error(`expected ${t}, got ${tok.t}`);
    return tok;
  }

  parseOr(): RuleNode {
    const parts = [this.parseAnd()];
    while (this.peek()?.t === "or") {
      this.next();
      parts.push(this.parseAnd());
    }
    if (parts.length === 1) return parts[0];
    return { kind: "group", id: uid(), combinator: "or", children: parts };
  }

  private parseAnd(): RuleNode {
    const parts = [this.parseTerm()];
    while (this.peek()?.t === "and") {
      this.next();
      parts.push(this.parseTerm());
    }
    if (parts.length === 1) return parts[0];
    return { kind: "group", id: uid(), combinator: "and", children: parts };
  }

  private parseTerm(): RuleNode {
    const tk = this.peek();
    if (!tk) throw new Error("unexpected end");
    if (tk.t === "lparen") {
      this.next();
      const inner = this.parseOr();
      this.expect("rparen");
      return inner;
    }
    if (tk.t === "not") {
      this.next();
      if (this.peek()?.t === "lparen") {
        // `not ( <field-or-lower> in [...] )` → none_of / not_equals
        this.next();
        const cond = this.parseSimpleLeaf();
        this.expect("rparen");
        if (cond.op === "any_of") cond.op = "none_of";
        else if (cond.op === "equals") cond.op = "not_equals";
        else throw new Error("unsupported negation");
        return cond;
      }
      // `not contains(...)` → not_contains ; `not is_one_off` → is_false
      const cond = this.parseSimpleLeaf();
      if (cond.op === "contains" && cond.values.length === 1) cond.op = "not_contains";
      else if (cond.field === "is_one_off" && cond.op === "is_true") cond.op = "is_false";
      else throw new Error("unsupported negation");
      return cond;
    }
    return this.parseSimpleLeaf();
  }

  // Parse one positive leaf into a Condition (no leading `not`).
  private parseSimpleLeaf(): Condition {
    const tk = this.peek();
    if (!tk || tk.t !== "id") throw new Error("expected leaf");
    if (FUNCS.has(tk.v)) return this.parseCall(tk.v);
    return this.parseFieldLeaf(tk.v);
  }

  private parseCall(name: string): Condition {
    this.next(); // function name
    if (name === "lower") {
      // lower(F) in [strings]  → text equals
      this.expect("lparen");
      const f = this.fieldFromId(this.expectId(), "text");
      this.expect("rparen");
      if (this.peek()?.t !== "in") throw new Error("expected in");
      this.next();
      const arr = this.parseArray();
      if (arr.kind !== "list") throw new Error("expected list");
      return this.cond(f, "equals", arr.strings());
    }
    if (name === "matches") {
      // matches(F, 'regex')
      this.expect("lparen");
      const f = this.fieldFromId(this.expectId(), "text");
      this.expect("comma");
      const s = this.expectStr();
      this.expect("rparen");
      return this.cond(f, "regex", [s]);
    }
    if (name === "contains" || name === "startsWith" || name === "endsWith") {
      // fn(lower(F), 'value')
      this.expect("lparen");
      const inner = this.expectId();
      if (inner !== "lower") throw new Error("expected lower()");
      this.expect("lparen");
      const f = this.fieldFromId(this.expectId(), "text");
      this.expect("rparen");
      this.expect("comma");
      const s = this.expectStr();
      this.expect("rparen");
      const op = name === "contains" ? "contains" : name === "startsWith" ? "starts_with" : "ends_with";
      return this.cond(f, op, [s]);
    }
    throw new Error(`unsupported function ${name}`);
  }

  private parseFieldLeaf(id: string): Condition {
    this.next(); // field identifier
    // Bare identifier → direction / boolean.
    const term = this.peek()?.t;
    const isTerminator =
      term === undefined || term === "and" || term === "or" || term === "rparen" || term === "comma" || term === "rbracket";
    if (isTerminator) {
      if (id === "is_income") return this.cond(fieldDef("direction")!, "is_income", []);
      if (id === "is_expense") return this.cond(fieldDef("direction")!, "is_expense", []);
      const f = fieldByZen(id);
      if (f?.type === "bool") return this.cond(f, "is_true", []);
      throw new Error(`bare identifier ${id}`);
    }
    if (term === "in") {
      this.next();
      const f = fieldByZen(id);
      if (!f) throw new Error(`unknown field ${id}`);
      const arr = this.parseArray();
      if (arr.kind === "interval") {
        if (f.type !== "money" && f.type !== "int") throw new Error("interval on non-numeric");
        return this.cond(f, "between", [arr.a, arr.b]);
      }
      if (f.type !== "enum" && f.type !== "ref") throw new Error("list on non-choice");
      return this.cond(f, "any_of", arr.strings());
    }
    // field <op> value
    const op = this.next();
    if (op.t !== "op") throw new Error("expected operator");
    const rhs = this.next();
    const f = fieldByZen(id);
    if (!f) throw new Error(`unknown field ${id}`);
    if (rhs.t === "null") {
      if (f.type !== "ref") throw new Error("null on non-ref");
      if (op.v === "==") return this.cond(f, "not_set", []);
      if (op.v === "!=") return this.cond(f, "is_set", []);
      throw new Error("bad null comparison");
    }
    if (rhs.t === "str") {
      if (f.type === "text" && rhs.v === "") {
        if (op.v === "==") return this.cond(f, "empty", []);
        if (op.v === "!=") return this.cond(f, "not_empty", []);
      }
      throw new Error("unsupported string comparison");
    }
    if (rhs.t === "num") {
      if (f.type !== "money" && f.type !== "int") throw new Error("number on non-numeric");
      const map: Record<string, string> = { "==": "eq", "!=": "neq", ">": "gt", ">=": "gte", "<": "lt", "<=": "lte" };
      const key = map[op.v];
      if (!key) throw new Error("bad numeric operator");
      return this.cond(f, key, [rhs.v]);
    }
    throw new Error("unsupported comparison");
  }

  private parseArray():
    | { kind: "list"; items: Tok[]; strings(): string[] }
    | { kind: "interval"; a: string; b: string } {
    this.expect("lbracket");
    const first = this.next();
    if (this.peek()?.t === "range") {
      this.next();
      const second = this.next();
      this.expect("rbracket");
      if (first.t !== "num" || second.t !== "num") throw new Error("bad interval");
      return { kind: "interval", a: first.v, b: second.v };
    }
    const items: Tok[] = [first];
    while (this.peek()?.t === "comma") {
      this.next();
      items.push(this.next());
    }
    this.expect("rbracket");
    return {
      kind: "list",
      items,
      strings: () =>
        items.map((it) => {
          if (it.t === "str") return it.v;
          if (it.t === "num") return it.v;
          throw new Error("bad list element");
        }),
    };
  }

  private expectId(): string {
    const t = this.next();
    if (t.t !== "id") throw new Error("expected identifier");
    return t.v;
  }
  private expectStr(): string {
    const t = this.next();
    if (t.t !== "str") throw new Error("expected string");
    return t.v;
  }
  private fieldFromId(id: string, want: FieldType): FieldDef {
    const f = fieldByZen(id);
    if (!f || f.type !== want) throw new Error(`expected ${want} field, got ${id}`);
    return f;
  }
  private cond(f: FieldDef, op: string, values: string[]): Condition {
    return { kind: "condition", id: uid(), field: f.key, op, values };
  }
}

/** Collapse an OR of single-value `contains` on one field into one multi-value row,
 *  so the seed's `(contains(..a) or contains(..b) or contains(..c))` round-trips as a
 *  single "Description contains a, b, c" condition. */
function collapse(n: RuleNode): RuleNode {
  if (n.kind !== "group") return n;
  n.children = n.children.map(collapse);
  if (
    n.combinator === "or" &&
    n.children.length > 1 &&
    n.children.every(
      (ch): ch is Condition =>
        ch.kind === "condition" &&
        ch.op === "contains" &&
        ch.values.length === 1 &&
        ch.field === (n.children[0] as Condition).field,
    )
  ) {
    return {
      kind: "condition",
      id: uid(),
      field: (n.children[0] as Condition).field,
      op: "contains",
      values: n.children.map((ch) => (ch as Condition).values[0]),
    };
  }
  return n;
}

const asRoot = (n: RuleNode): Group =>
  n.kind === "group" ? n : { kind: "group", id: uid(), combinator: "and", children: [n] };

/** Parse a Zen expression into a builder tree, or `null` if it uses anything outside
 *  the builder's vocabulary (the caller then falls back to a raw-expression editor). */
export function parse(expr: string): Group | null {
  const src = expr.trim();
  if (!src) return null;
  try {
    const toks = tokenize(src);
    if (!toks.length) return null;
    const p = new Parser(toks);
    const node = p.parseOr();
    if (!p.eof()) return null;
    const root = asRoot(collapse(node));
    return root.children.length ? root : null;
  } catch {
    return null;
  }
}
