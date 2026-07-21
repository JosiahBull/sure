<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Schemas } from "../lib/api";

  let currencies = $state<Schemas["Currency"][]>([]);
  let settings = $state<Schemas["Settings"] | null>(null);
  let notice = $state<string | null>(null);
  let error = $state<string | null>(null);
  let importText = $state("");

  async function load() {
    const [c, s] = await Promise.all([api.GET("/api/currencies", {}), api.GET("/api/settings", {})]);
    currencies = c.data ?? [];
    settings = s.data ?? null;
  }
  onMount(load);

  async function setBase(code: string) {
    await api.PUT("/api/settings", { body: { base_currency_code: code } });
    notice = "Base currency updated.";
    load();
  }

  async function exportConfig() {
    const { data } = await api.GET("/api/config/export", {});
    if (!data) return;
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `sure-config-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
    notice = "Config exported.";
  }
  async function importConfig() {
    error = null;
    let parsed: unknown;
    try {
      parsed = JSON.parse(importText);
    } catch {
      error = "That isn't valid JSON.";
      return;
    }
    // The snapshot body is an opaque JSON blob (serde_json::Value on the backend).
    const { error: e } = await api.POST("/api/config/import", { body: parsed as never });
    if (e) {
      error = "Import failed — check the snapshot.";
      return;
    }
    notice = "Config imported.";
    importText = "";
    load();
  }
</script>

<h1 style="font-size:20px;margin-bottom:14px">Preferences</h1>
{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}
{#if notice}<div class="badge" style="margin-bottom:12px">{notice}</div>{/if}

<div class="grid" style="gap:14px">
  <section class="card">
    <h2>Base currency</h2>
    <p class="muted small" style="margin-top:0">Reports are normalised into this currency.</p>
    <div class="row" style="gap:10px">
      <select
        class="select"
        style="width:auto"
        value={settings?.base_currency_code ?? "NZD"}
        onchange={(e) => setBase(e.currentTarget.value)}
      >
        {#each currencies as c}<option value={c.code}>{c.code} — {c.name}</option>{/each}
      </select>
    </div>
  </section>

  <section class="card">
    <h2>Backup &amp; restore</h2>
    <p class="muted small" style="margin-top:0">
      Export the whole configuration and data as JSON (handy for rapid dev iteration), or
      paste a snapshot to restore. Import <strong>replaces everything</strong>.
    </p>
    <div class="row" style="margin-bottom:10px">
      <button class="btn btn-primary" onclick={exportConfig}>Export JSON</button>
    </div>
    <textarea
      class="mono"
      rows="3"
      placeholder="Paste a snapshot JSON here to restore…"
      bind:value={importText}
    ></textarea>
    <div class="row" style="justify-content:flex-end;margin-top:8px">
      <button class="btn btn-danger" onclick={importConfig} disabled={!importText.trim()}>
        Import &amp; replace
      </button>
    </div>
  </section>
</div>
