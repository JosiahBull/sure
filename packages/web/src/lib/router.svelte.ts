// Minimal hash router — bulletproof for a statically-served PWA (no server rewrites
// needed) and dependency-free.

function currentPath(): string {
  return window.location.hash.replace(/^#/, "") || "/";
}

export const router = $state({ path: currentPath() });

window.addEventListener("hashchange", () => {
  router.path = currentPath();
});

export function navigate(path: string): void {
  if (currentPath() === path) return;
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
  const [path, qs] = router.path.split("?");
  const params = new URLSearchParams(qs ?? "");
  if (value === null) params.delete(key);
  else params.set(key, value);
  const next = params.toString();
  navigate(next ? `${path}?${next}` : path);
}
