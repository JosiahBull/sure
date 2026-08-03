// Shared category-tree helpers. Several pages need the same two things — "this category and
// everything under it" for filtering, and an ordered, indented list for a <select> — and at
// three levels a bare "Partly Group" is ambiguous, so the label helpers qualify a name with
// its ancestors the way the previous app's `Category#name_with_parent` did.
//
// Consolidates the copy of childrenOf/categorySubtree that lived in Transactions.svelte.
import type { Schemas } from "./api";

export type Category = Schemas["Category"];

/** Guard every ancestor walk, matching `sure_app::reports::Categories`. The API rejects a
 *  cycle on write, but a hand-edited database shouldn't be able to hang the page. */
const MAX_HOPS = 64;

/**
 * How many levels of nesting the API accepts, top level included.
 *
 * Mirrors `sure_core::MAX_CATEGORY_DEPTH`, which `sure_dal::categories::validate` enforces.
 * It isn't on the wire — it's a fixed product decision rather than per-install config — so
 * the client carries its own copy purely to grey out parents that would be rejected. The
 * server stays the authority; this only avoids offering a choice that returns a 422.
 */
export const MAX_CATEGORY_DEPTH = 3;

/** parent id -> child ids, in the source list's own (sort_order, name) order. */
export function childrenOf(cats: Category[]): Map<number, number[]> {
  const m = new Map<number, number[]>();
  for (const c of cats) {
    if (c.parent_id == null) continue;
    const arr = m.get(c.parent_id);
    if (arr) arr.push(c.id);
    else m.set(c.parent_id, [c.id]);
  }
  return m;
}

/**
 * `id` plus every descendant. The reports roll spend up the tree, so filtering the
 * transaction list by a parent has to include its children's rows to agree with them.
 */
export function subtreeIds(cats: Category[], id: number): Set<number> {
  const kids = childrenOf(cats);
  const out = new Set<number>([id]);
  const stack = [id];
  while (stack.length) {
    for (const child of kids.get(stack.pop()!) ?? []) {
      if (!out.has(child)) {
        out.add(child);
        stack.push(child);
      }
    }
  }
  return out;
}

/** Ancestors root-first, ending at `id` itself. Empty if `id` isn't in `cats`. */
export function chainOf(cats: Category[], id: number): Category[] {
  const byId = new Map(cats.map((c) => [c.id, c]));
  const chain: Category[] = [];
  let cur = byId.get(id);
  for (let hop = 0; cur && hop < MAX_HOPS; hop++) {
    chain.push(cur);
    const parentId = cur.parent_id;
    if (parentId == null || chain.some((c) => c.id === parentId)) break;
    cur = byId.get(parentId);
  }
  return chain.reverse();
}

/** How deep `id` sits: 0 for a top-level category. */
export function depthOf(cats: Category[], id: number): number {
  return Math.max(0, chainOf(cats, id).length - 1);
}

/** How many levels sit *below* `id`: 0 for a leaf. What a re-parent has to make room for. */
export function subtreeHeight(cats: Category[], id: number): number {
  const kids = childrenOf(cats);
  let height = 0;
  let frontier = [id];
  const seen = new Set(frontier);
  for (let hop = 0; hop < MAX_HOPS && frontier.length; hop++) {
    const next = frontier.flatMap((p) => (kids.get(p) ?? []).filter((c) => !seen.has(c)));
    next.forEach((c) => seen.add(c));
    if (next.length) height++;
    frontier = next;
  }
  return height;
}

/** `id`'s top-level ancestor — the key its whole branch shares a colour family under. */
export function rootIdOf(cats: Category[], id: number): number {
  return chainOf(cats, id)[0]?.id ?? id;
}

/** "Income > Employment > Partly Group", for chips, tooltips and single-line displays. */
export function qualifiedName(cats: Category[], id: number, sep = " > "): string {
  const chain = chainOf(cats, id);
  return chain.length ? chain.map((c) => c.name).join(sep) : "";
}

export interface CategoryOption {
  id: number;
  name: string;
  depth: number;
  /** Indented display text — see {@link categoryOptions}. */
  label: string;
}

/**
 * Depth-first, parents before children, each label indented to show its level.
 *
 * The indent uses non-breaking spaces because `<option>` collapses ordinary leading
 * whitespace, and `<optgroup>` — the markup answer — only nests one level, which is one
 * short of what the category tree now allows.
 */
export function categoryOptions(cats: Category[], opts?: { exclude?: Set<number> }): CategoryOption[] {
  const kids = childrenOf(cats);
  const byId = new Map(cats.map((c) => [c.id, c]));
  const out: CategoryOption[] = [];
  const walk = (id: number, depth: number) => {
    if (opts?.exclude?.has(id)) return;
    const cat = byId.get(id);
    if (!cat) return;
    out.push({
      id,
      name: cat.name,
      depth,
      label: `${"  ".repeat(depth)}${depth ? "↳ " : ""}${cat.name}`,
    });
    for (const child of kids.get(id) ?? []) walk(child, depth + 1);
  };
  for (const c of cats) if (c.parent_id == null) walk(c.id, 0);
  return out;
}
