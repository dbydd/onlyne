<script lang="ts">
  // The composed empty state. An empty panel that says what it is empty of,
  // and how to fill it, is the difference between "nothing here" and "nothing
  // works" — the two readings this surface must never confuse.
  import type { Component, Snippet } from 'svelte';

  interface Props {
    /// A phosphor icon component, e.g. `import Graph from 'phosphor-svelte/lib/Graph'`.
    icon?: Component<Record<string, unknown>>;
    title: string;
    hint?: string;
    children?: Snippet;
  }

  let { icon: Icon, title, hint = '', children }: Props = $props();
</script>

<div class="empty">
  {#if Icon}
    <span class="mark"><Icon size="18" /></span>
  {/if}
  <p class="title">{title}</p>
  {#if hint}
    <p class="hint">{hint}</p>
  {/if}
  {#if children}
    <div class="act">{@render children()}</div>
  {/if}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 26px 18px;
    text-align: center;
  }
  .mark {
    color: var(--ink-4);
  }
  .title {
    font-size: var(--fs-12);
    font-weight: 500;
    color: var(--ink-2);
    text-wrap: balance;
  }
  .hint {
    max-width: 46ch;
    font-size: var(--fs-11);
    color: var(--ink-3);
    text-wrap: pretty;
  }
  .act {
    margin-top: 4px;
    display: flex;
    gap: var(--s-2);
  }
</style>
