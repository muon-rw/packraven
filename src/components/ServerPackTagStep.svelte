<script lang="ts">
  import { openUrl } from '@tauri-apps/plugin-opener'

  let { page }: { page: string } = $props()
</script>

<div class="server-tag-step">
  <p>
    <strong>Tag the server pack on CurseForge.</strong> It's not currently possible to set <span class="ui-label">Additional File Info</span> to <span class="ui-label">Server Pack</span> automatically, so you'll need to set it on CurseForge yourself.
  </p>
  <p><button onclick={() => openUrl(page)}>Open the server pack on CurseForge</button></p>
</div>

<style>
  .server-tag-step {
    display: grid;
    gap: 6px;
    margin-top: 2px;
    padding-top: 8px;
    border-top: 1px solid var(--border, var(--line-strong));
  }

  .ui-label {
    font-weight: 600;
  }
</style>
