<script lang="ts">
  // The chrome every data panel shares: a titled header with a count, and one
  // scrolling body. The panels' row markup stays in each panel; only this
  // shell is shared, because the shell is the only thing that must agree.
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    count?: number;
    actions?: Snippet;
    children: Snippet;
  }

  let { title, count, actions, children }: Props = $props();
</script>

<aside class="panel">
  <header>
    <span class="title">{title}</span>
    {#if count != null}
      <span class="count">{count}</span>
    {/if}
    {#if actions}
      {@render actions()}
    {/if}
  </header>
  <div class="body">
    {@render children()}
  </div>
</aside>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--panel, #10131a);
    border-right: 1px solid var(--line, #232935);
  }
  aside.panel:last-child {
    border-right: none;
    border-left: 1px solid var(--line, #232935);
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line, #232935);
  }
  .title {
    flex: 1;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--faint, #565e6c);
    user-select: none;
  }
  .count {
    font-family: var(--mono, ui-monospace, monospace);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    color: var(--muted, #7d8593);
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .body::-webkit-scrollbar {
    width: 6px;
  }
  .body::-webkit-scrollbar-thumb {
    background: #2b323e;
    border-radius: 3px;
  }
</style>
