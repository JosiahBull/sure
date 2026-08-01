// Shared household roster. Almost every surface that shows money eventually wants to put a
// name or a colour next to it, and the list is two rows — so it's loaded once here and read
// everywhere, the same way `balances.svelte.ts` shares the balances snapshot.
import { api, colorFor, type Schemas } from "./api";

export type Person = Schemas["Person"];
export type Ownership = Schemas["Ownership"];

export const people = $state({
  list: [] as Person[],
  loaded: false,
  error: null as string | null,
});

export async function refresh(): Promise<void> {
  const { data, error } = await api.GET("/api/people", {});
  people.list = data ?? [];
  people.error = error ? "Failed to load the household." : null;
  people.loaded = true;
}

/** Load once per session unless a caller explicitly asks for fresh data. */
export async function ensureLoaded(): Promise<void> {
  if (!people.loaded) await refresh();
}

export function personById(id: number): Person | undefined {
  return people.list.find((p) => p.id === id);
}

/** A person's own colour, falling back to the same id-derived palette categories use. */
export function personColor(person: Person): string {
  return person.color ?? colorFor(person.id);
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0].charAt(0).toUpperCase();
  return (parts[0].charAt(0) + parts[parts.length - 1].charAt(0)).toUpperCase();
}

/** How an account's ownership reads in the UI. */
export function ownershipLabel(ownership: Ownership): string {
  switch (ownership.kind) {
    case "person":
      return personById(ownership.person_id)?.name ?? "Unknown person";
    case "joint":
      return "Joint";
  }
}

export function ownershipColor(ownership: Ownership): string | null {
  switch (ownership.kind) {
    case "person": {
      const person = personById(ownership.person_id);
      return person ? personColor(person) : null;
    }
    case "joint":
      return null;
  }
}

/** Stable string form, for `<select>` values and grouping keys. */
export function ownershipKey(ownership: Ownership): string {
  switch (ownership.kind) {
    case "person":
      return `person:${ownership.person_id}`;
    case "joint":
      return "joint";
  }
}

/**
 * The inverse of {@link ownershipKey}. Falls back to joint for an unrecognised key: it names
 * no individual, so the worst case is a shared account, never someone else's money labelled
 * as yours.
 */
export function ownershipFromKey(key: string): Ownership {
  if (key.startsWith("person:")) {
    const person_id = Number(key.slice("person:".length));
    if (Number.isFinite(person_id)) return { kind: "person", person_id };
  }
  return { kind: "joint" };
}

/** The options an owner `<select>` offers, in a fixed order. */
export function ownershipOptions(): { key: string; label: string }[] {
  return [
    ...people.list.map((p) => ({ key: `person:${p.id}`, label: p.name })),
    { key: "joint", label: "Joint (shared)" },
  ];
}

/**
 * The owner a form should start on when it has no existing value: the first household
 * member, or joint if the roster somehow hasn't loaded. Every account has to name one, so
 * there's no "unset" option to fall back to — the select is pre-filled and the user changes
 * it, which is also how it reads with a two-person household.
 */
export function defaultOwnershipKey(): string {
  return people.list.length > 0 ? `person:${people.list[0].id}` : "joint";
}

/** People the app invented to satisfy the ownership requirement, still waiting to be named. */
export function placeholders(): Person[] {
  return people.list.filter((p) => p.placeholder);
}
