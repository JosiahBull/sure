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
