<script lang="ts">
  // The chrome every inspector shares: one eyebrow, one name, one close. The
  // bodies differ so much that only this shell is worth agreeing on.
  import X from 'phosphor-svelte/lib/X';
  import type { Snippet } from 'svelte';
  import { ui } from '../lib/state/ui.svelte';

  interface Props {
    eyebrow: string;
    title: string;
    subtitle?: string;
    /// An op is in flight for this panel, so it says so rather than looking idle.
    busy?: boolean;
    actions?: Snippet;
    children: Snippet;
    footer?: Snippet;
  }

  let { eyebrow, title, subtitle = '', busy = false, actions, children, footer }: Props = $props();
</script>

<div class="frame" aria-busy={busy}>
  <header>
    <div class="titles">
      <span class="eyebrow">{eyebrow}</span>
      <h2 class="trunc">{title}</h2>
      {#if subtitle}
        <p class="sub trunc">{subtitle}</p>
      {/if}
    </div>
    {#if actions}
      {@render actions()}
    {/if}
    <button class="btn icon ghost sm" aria-label="close the inspector" onclick={() => (ui.selection = null)}>
      <X size="12" weight="bold" />
    </button>
  </header>

  <div class="content">
    {@render children()}
  </div>

  {#if footer}
    <footer>
      {@render footer()}
    </footer>
  {/if}
</div>

<style>
  .frame {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--panel);
  }
  header {
    flex: none;
    display: flex;
    align-items: flex-start;
    gap: var(--s-2);
    padding: var(--s-3) var(--s-3) 10px;
    border-bottom: 1px solid var(--line);
  }
  .titles {
    flex: 1;
    min-width: 0;
  }
  h2 {
    margin-top: 2px;
    font-size: var(--fs-15);
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .sub {
    margin-top: 3px;
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .content {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  footer {
    flex: none;
    padding: 10px var(--s-3);
    border-top: 1px solid var(--line);
    background: var(--bg);
  }
</style>
