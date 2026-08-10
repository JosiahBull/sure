<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Schemas } from "../lib/api";

  type McpMode = Schemas["McpMode"];

  let currencies = $state<Schemas["Currency"][]>([]);
  let settings = $state<Schemas["SettingsView"] | null>(null);
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

  // What the environment permits, and therefore which options this page may offer. `off`
  // (the default, `SURE_MCP` unset) means the endpoint is not mounted at all and the control
  // below is disabled — showing an enabled control that silently does nothing would be worse
  // than showing a disabled one that says why.
  const MCP_LABELS: Record<McpMode, string> = {
    off: "Off",
    read: "Read only",
    write: "Read and write",
  };
  const MCP_ORDER: McpMode[] = ["off", "read", "write"];
  const mcpAllowed = $derived(
    MCP_ORDER.slice(0, MCP_ORDER.indexOf(settings?.mcp_ceiling ?? "off") + 1),
  );
  const mcpLocked = $derived((settings?.mcp_ceiling ?? "off") === "off");

  async function setMcpMode(mode: McpMode) {
    error = null;
    const { error: e } = await api.PUT("/api/settings", {
      body: {
        base_currency_code: settings?.base_currency_code ?? "NZD",
        mcp_mode: mode,
      },
    });
    if (e) {
      error = "Could not change agent access — the server refused it.";
      return;
    }
    notice =
      mode === "off"
        ? "Agent access turned off."
        : `Agent access set to ${MCP_LABELS[mode].toLowerCase()}.`;
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
      error = "Restore failed — check the snapshot.";
      return;
    }
    notice = "Backup restored.";
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
    <h2>Agent access (MCP)</h2>
    <p class="muted small" style="margin-top:0">
      Lets an AI assistant read this ledger over
      <a href="https://modelcontextprotocol.io" target="_blank" rel="noreferrer">MCP</a>, and —
      on the last setting — file transactions and write rules.
      <strong>Turning this on sends transaction descriptions, which contain account numbers
      and payee names, to whichever model the assistant runs.</strong>
    </p>
    {#if mcpLocked}
      <p class="muted small">
        Disabled on this server. Set the <code>SURE_MCP</code> environment variable to
        <code>read</code> or <code>write</code> and restart to make this choice available.
      </p>
    {/if}
    <div class="row" style="gap:10px">
      <select
        class="select"
        style="width:auto"
        disabled={mcpLocked}
        value={settings?.mcp_mode ?? "off"}
        onchange={(e) => setMcpMode(e.currentTarget.value as McpMode)}
      >
        {#each mcpAllowed as mode}<option value={mode}>{MCP_LABELS[mode]}</option>{/each}
      </select>
    </div>
    {#if !mcpLocked && settings}
      <p class="muted small" style="margin-bottom:0">
        {#if settings.mcp_effective === "off"}
          Nothing is being served. Connect with
          <code>claude mcp add --transport http sure {location.origin}/mcp</code> once enabled.
        {:else}
          Serving <strong>{MCP_LABELS[settings.mcp_effective].toLowerCase()}</strong> access at
          <code>{location.origin}/mcp</code>. Changes apply immediately.
          {#if settings.mcp_ceiling !== "write"}
            <code>SURE_MCP</code> caps this at <code>{settings.mcp_ceiling}</code>.
          {/if}
        {/if}
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Backup &amp; restore</h2>
    <p class="muted small" style="margin-top:0">
      Export the whole configuration and data as JSON (handy for rapid dev iteration), or
      paste a snapshot to restore. Restoring <strong>replaces everything</strong>.
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
        Restore from backup
      </button>
    </div>
  </section>
</div>
