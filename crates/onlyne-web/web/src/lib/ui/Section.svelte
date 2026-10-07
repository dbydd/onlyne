<script lang="ts">
  // A labelled block in the inspector: one heading, one body. `count` rides in
  // the header because a collapsed block should still say how much it holds.
  import CaretRight from 'phosphor-svelte/lib/CaretRight';
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    count?: number;
    open?: boolean;
    tone?: string;
    actions?: Snippet;
    children: Snippet;
  }

  let { title, count, open = $bindable(true), tone = '', actions, children }: Props = $props();
</script>

<section class="section">
  <header>
    <button class="head" onclick={() => (open = !open)} aria-expanded={open}>
      <span class="caret" class:open><CaretRight size="11" weight="bold" /></span>
      <span class="title">{title}</span>
      {#if count != null}
        <span class="count num">{count}</span>
      {/if}
    </button>
    {#if actions}
      {@render actions()}
    {/if}
  </header>
  {#if open}
    <div class="body">
      {@render children()}
    </div>
  {/if}
</section>

<style>
  .section {
    border-bottom: 1px solid var(--line-soft);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding-right: 8px;
  }
  .head {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 4px 8px 10px;
    border: 0;
    background: none;
    text-align: left;
  }
  .caret {
    display: inline-flex;
    flex: none;
    color: var(--ink-4);
    transition: transform var(--t-fast) var(--ease);
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .title {
    font-size: var(--fs-11);
    font-weight: 600;
    color: var(--ink-2);
    letter-spacing: 0.01em;
  }
  .count {
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-4);
  }
  .body {
    padding: 0 10px 10px;
  }
  .body :global(> .first) {
    margin-top: 0;
  }
</style>
