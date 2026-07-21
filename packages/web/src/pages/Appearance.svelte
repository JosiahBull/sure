<script lang="ts">
  import { theme, setTheme, resolvedTheme, type ThemePref } from "../lib/theme.svelte";

  const THEMES: { key: ThemePref; label: string; glyph: string }[] = [
    { key: "auto", label: "Auto", glyph: "◐" },
    { key: "light", label: "Light", glyph: "☀" },
    { key: "dark", label: "Dark", glyph: "☾" },
  ];
</script>

<h1 style="font-size:20px;margin-bottom:14px">Appearance</h1>

<section class="card">
  <h2>Theme</h2>
  <p class="muted small" style="margin-top:0">
    Auto follows your device{theme.pref === "auto" ? ` — currently ${resolvedTheme()}` : ""}.
  </p>
  <div class="segmented" role="group" aria-label="Theme">
    {#each THEMES as t}
      <button
        type="button"
        class="seg"
        class:active={theme.pref === t.key}
        aria-pressed={theme.pref === t.key}
        onclick={() => setTheme(t.key)}
      >
        <span aria-hidden="true">{t.glyph}</span>
        {t.label}
      </button>
    {/each}
  </div>
</section>

<style>
  .segmented {
    display: inline-flex;
    padding: 3px;
    gap: 3px;
    border-radius: var(--r);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
  }
  .seg {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 16px;
    border: none;
    border-radius: calc(var(--r) - 4px);
    background: transparent;
    color: var(--text-muted);
    font-family: inherit;
    font-size: 14px;
    font-weight: 550;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .seg:hover {
    color: var(--text);
  }
  .seg.active {
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 650;
  }
</style>
