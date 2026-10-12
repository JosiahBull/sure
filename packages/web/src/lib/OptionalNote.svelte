<script lang="ts">
  let {
    value = $bindable(""),
    open = $bindable(false),
    placeholder,
  }: { value?: string; open?: boolean; placeholder?: string } = $props();
</script>

<details class="form-optional" bind:open>
  <summary>Note (optional)</summary>
  <label>
    <span class="sr-only">Note (optional)</span>
    <input class="input" {placeholder} bind:value />
  </label>
</details>

<style>
  .form-optional {
    width: 100%;
    min-width: 0;
    margin: 4px 0 0;
    font-size: 13px;
  }
  summary {
    min-height: 44px;
    padding: 12px 0;
    text-align: left;
    color: var(--text-muted);
    cursor: pointer;
  }
  label { display: block; }
  input {
    width: 100%;
    min-width: 0;
    min-height: 44px;
    font-size: 16px;
    text-align: left;
  }
</style>
