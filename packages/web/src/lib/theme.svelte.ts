// Light/dark theme controller. The user's preference is one of three values;
// "auto" follows the OS via matchMedia. The resolved theme ("light" | "dark")
// is written to <html data-theme> — app.css keys both palettes off that
// attribute — and mirrored into the <meta name="theme-color"> so the mobile
// browser chrome (address bar / status bar) matches the page.
//
// index.html runs a tiny inline copy of this resolution before first paint to
// avoid a flash of the wrong theme; this module owns every change afterwards.

export type ThemePref = "auto" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "sure:theme";

// Keep these in sync with --bg in app.css and the inline script in index.html.
const THEME_COLOR: Record<ResolvedTheme, string> = {
  dark: "#0b0b0b",
  light: "#f7f7f7",
};

const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");

function readPref(): ThemePref {
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "auto" ? stored : "auto";
}

// Reactive so the UI can highlight the active choice and reflect the live
// system value while on "auto".
export const theme = $state<{ pref: ThemePref; systemDark: boolean }>({
  pref: readPref(),
  systemDark: darkQuery.matches,
});

/** The theme actually being shown, resolving "auto" against the system. */
export function resolvedTheme(): ResolvedTheme {
  if (theme.pref === "auto") return theme.systemDark ? "dark" : "light";
  return theme.pref;
}

function apply(): void {
  const resolved = resolvedTheme();
  document.documentElement.setAttribute("data-theme", resolved);
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute("content", THEME_COLOR[resolved]);
}

/** Set and persist the user's preference, applying it immediately. */
export function setTheme(pref: ThemePref): void {
  theme.pref = pref;
  localStorage.setItem(STORAGE_KEY, pref);
  apply();
}

// Track OS changes so "auto" flips live without a reload.
darkQuery.addEventListener("change", (e) => {
  theme.systemDark = e.matches;
  if (theme.pref === "auto") apply();
});

// Reconcile with the pre-paint inline script (and cover other tabs).
apply();
