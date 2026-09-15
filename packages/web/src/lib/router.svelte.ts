// Minimal hash router — bulletproof for a statically-served PWA (no server rewrites
// needed) and dependency-free.

function currentPath(): string {
  return window.location.hash.replace(/^#/, "") || "/";
}

export const router = $state({ path: currentPath() });

window.addEventListener("hashchange", () => {
  router.path = currentPath();
});

/**
 * Go to a path.
 *
 * `replace` swaps the current history entry instead of pushing a new one — which is what a
 * *filter* change wants: picking four time ranges in a row should leave one entry to go back
 * from, not four. Assigning to `location.hash` always pushes, so the replace path goes through
 * `history.replaceState`, which does not fire `hashchange` and therefore has to update `router`
 * itself.
 */
export function navigate(path: string, opts: { replace?: boolean } = {}): void {
  if (currentPath() === path) return;
  if (opts.replace) {
    history.replaceState(history.state, "", `#${path}`);
    router.path = path;
    return;
  }
  window.location.hash = path;
}

/**
 * The hash's query string, parsed fresh on each call.
 *
 * A function rather than an exported `$derived` so the module does not have to own the
 * subscription: reading `router.path` happens *inside* the caller's own `$derived`, which is what
 * makes it reactive — the same arrangement `people.list` already relies on.
 *
 * This is a query on the *hash*, so `App.svelte` never sees it: it keys the active page on
 * `router.path.split("?")[0]` and remounts with `{#key activePath}`, so changing a param here
 * updates state without tearing down and refetching the page. A path segment would have done
 * the opposite.
 */
export function queryParams(): URLSearchParams {
  return new URLSearchParams(router.path.split("?")[1] ?? "");
}

/** Set one hash query param — or drop it, with `null` — leaving the path and the others alone. */
export function setQueryParam(key: string, value: string | null): void {
  setQueryParams({ [key]: value });
}

/**
 * Set several hash query params at once, leaving the path and any others alone.
 *
 * One call rather than several because they would otherwise be several history entries — and
 * because a period is two params that have to move together: writing `range` before clearing
 * `start`/`end` would leave a moment where the URL says both, and whoever read it in between
 * would believe the wrong one.
 */
export function setQueryParams(
  values: Record<string, string | null>,
  opts: { replace?: boolean } = {},
): void {
  const [path, qs] = router.path.split("?");
  const params = new URLSearchParams(qs ?? "");
  for (const [key, value] of Object.entries(values)) {
    if (value === null) params.delete(key);
    else params.set(key, value);
  }
  const next = params.toString();
  navigate(next ? `${path}?${next}` : path, opts);
}
