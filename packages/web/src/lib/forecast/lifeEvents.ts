// Shared vocabulary for the life-events editor: labels, kind-seeded defaults, and the cycle guard.

import type { Schemas } from "../api";

export type ForecastEvent = Schemas["ForecastEvent"];
export type LifeEventKind = Schemas["LifeEventKind"];
export type LifeEffectSpec = Schemas["LifeEffectSpec"];
export type SaveRelation = Schemas["SaveForecastEventRelation"];

export const EVENT_KINDS: { kind: LifeEventKind; label: string; icon: string }[] = [
  { kind: "promotion", label: "Promotion", icon: "↗" },
  { kind: "child", label: "Child", icon: "◍" },
  { kind: "career_break", label: "Career break", icon: "⏸" },
  { kind: "job_start", label: "New job", icon: "→" },
  { kind: "job_end", label: "Job ends", icon: "⤫" },
  { kind: "adjustment", label: "Certain change", icon: "=" },
  { kind: "custom", label: "Something else", icon: "•" },
];

export function kindLabel(k: LifeEventKind): string {
  return EVENT_KINDS.find((x) => x.kind === k)?.label ?? "Event";
}
export function kindIcon(k: LifeEventKind): string {
  return EVENT_KINDS.find((x) => x.kind === k)?.icon ?? "•";
}

/**
 * What each kind of event usually does, pre-filled the moment the kind is picked.
 *
 * Composing effect rows from an empty list is a task nobody finishes; correcting two pre-filled ones
 * is a task everybody finishes. The figures are deliberately round and obviously editable — a
 * starting point to argue with, not an estimate being asserted.
 *
 * The `switch` is exhaustive with no `default`, so an eighth kind is a compile error right here
 * rather than an event that silently seeds nothing. That is CLAUDE.md rule 2 carried across the
 * language boundary, and it is the reason this is a switch and not a lookup table.
 */
export function seedEffects(
  kind: LifeEventKind,
  ctx: { personId: number | null; streamId: number | null; categoryId: number | null }
): LifeEffectSpec[] {
  const stream = ctx.streamId;
  switch (kind) {
    case "promotion":
      return stream === null
        ? []
        : [
            {
              kind: "income_step",
              income_stream_id: stream,
              amount: { basis: "percent", rate_bps: 1_000 },
            } as LifeEffectSpec,
          ];
    case "child": {
      const out: LifeEffectSpec[] = [];
      if (ctx.categoryId !== null) {
        out.push({
          kind: "recurring_delta",
          category_id: ctx.categoryId,
          amount_minor: 1_200_00,
          // Daycare does not start the day a child is born, and it does not last forever.
          delay_months: 12,
          ramp_months: 3,
          duration_months: 48,
        } as LifeEffectSpec);
      }
      if (ctx.personId !== null) {
        out.push({
          kind: "income_pause",
          person_id: ctx.personId,
          months: 12,
          replacement_rate_bps: 6_000,
        } as LifeEffectSpec);
      }
      return out;
    }
    case "career_break":
      return ctx.personId === null
        ? []
        : [
            {
              kind: "income_pause",
              person_id: ctx.personId,
              months: 6,
              replacement_rate_bps: 0,
            } as LifeEffectSpec,
          ];
    case "job_start":
      return stream === null
        ? []
        : [{ kind: "income_start", income_stream_id: stream } as LifeEffectSpec];
    case "job_end":
      return stream === null
        ? []
        : [{ kind: "income_end", income_stream_id: stream } as LifeEffectSpec];
    case "adjustment":
    case "custom":
      return [];
  }
}

/** Sensible probability and spread for a kind — a certainty is certain, a guess is a guess. */
export function seedTiming(kind: LifeEventKind): { probability_bps: number; spread: number } {
  switch (kind) {
    case "adjustment":
      return { probability_bps: 10_000, spread: 0 };
    case "child":
      return { probability_bps: 8_000, spread: 24 };
    case "promotion":
      return { probability_bps: 7_000, spread: 18 };
    case "career_break":
      return { probability_bps: 8_000, spread: 12 };
    case "job_start":
    case "job_end":
      return { probability_bps: 9_000, spread: 6 };
    case "custom":
      return { probability_bps: 10_000, spread: 0 };
  }
}

/**
 * The events `self` may legally depend on.
 *
 * All of them, minus itself, minus every event that already reaches `self` through the existing
 * edges — those are precisely the choices that close a loop, so removing them from the picker makes
 * a cycle **unbuildable by clicking**. Validation-after-the-fact is a worse experience than a shorter
 * list, and it is the only approach that stays usable past two or three events.
 *
 * The server still checks and answers 409. Two tabs open on the same household is enough to make
 * this list stale, and the client is not the authority on anything.
 */
export function eligibleTargets(all: ForecastEvent[], selfId: number | null): ForecastEvent[] {
  if (selfId === null) return all;
  const dependents = new Map<number, number[]>();
  for (const e of all) {
    for (const r of e.relations) {
      const list = dependents.get(r.depends_on_event_id);
      if (list) list.push(e.id);
      else dependents.set(r.depends_on_event_id, [e.id]);
    }
  }
  const blocked = new Set([selfId]);
  const stack = [selfId];
  while (stack.length) {
    for (const d of dependents.get(stack.pop()!) ?? []) {
      if (!blocked.has(d)) {
        blocked.add(d);
        stack.push(d);
      }
    }
  }
  return all.filter((e) => !blocked.has(e.id));
}

/** A one-line summary of what an effect does, for the collapsed row. */
export function effectSummary(
  e: LifeEffectSpec,
  names: {
    stream: (id: number) => string;
    person: (id: number) => string;
    category: (id: number) => string;
    account: (id: number) => string;
    money: (minor: number) => string;
  }
): string {
  const spec = e as unknown as Record<string, unknown>;
  const kind = spec.kind as string;
  switch (kind) {
    case "income_step": {
      const amount = spec.amount as { basis: string; rate_bps?: number; annual_amount_minor?: number };
      const what =
        amount.basis === "percent"
          ? `${(amount.rate_bps ?? 0) >= 0 ? "+" : ""}${((amount.rate_bps ?? 0) / 100).toFixed(1)}%`
          : `to ${names.money(amount.annual_amount_minor ?? 0)}/yr`;
      return `${names.stream(spec.income_stream_id as number)} ${what}`;
    }
    case "income_start":
      return `${names.stream(spec.income_stream_id as number)} starts`;
    case "income_end":
      return `${names.stream(spec.income_stream_id as number)} ends`;
    case "income_pause":
      return `${names.person(spec.person_id as number)} pauses ${spec.months} months at ${
        ((spec.replacement_rate_bps as number) ?? 0) / 100
      }% pay`;
    case "recurring_delta": {
      const dur = spec.duration_months ? ` for ${spec.duration_months} months` : " ongoing";
      const delay = (spec.delay_months as number) ? `, from +${spec.delay_months}mo` : "";
      return `${names.category(spec.category_id as number)} ${names.money(
        spec.amount_minor as number
      )}/mo${delay}${dur}`;
    }
    case "set_baseline":
    case "one_off_amount": {
      const target = spec.target as { kind: string; account_id?: number; category_id?: number };
      const where =
        target.kind === "account"
          ? names.account(target.account_id ?? 0)
          : names.category(target.category_id ?? 0);
      const verb = kind === "set_baseline" ? "becomes" : "one-off";
      return `${where} ${verb} ${names.money(spec.amount_minor as number)}`;
    }
    default:
      return kind;
  }
}
