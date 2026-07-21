<script lang="ts">
  import { ICONS } from "./icons";

  let { name, size = 20 }: { name: keyof typeof ICONS; size?: number } = $props();
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
  {@html ICONS[name] ?? ""}
</svg>
