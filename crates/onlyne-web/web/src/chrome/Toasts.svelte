<script lang="ts">
  // The notice stack. It sits in the bottom right corner over the dock, takes
  // its colour from the tone the toast was raised with, and moves nothing: the
  // stack is positioned, so a toast appearing cannot push a row of the surface
  // behind it out of place.
  import X from 'phosphor-svelte/lib/X';
  import { ui, type Toast } from '../lib/state/ui.svelte';

  function act(toast: Toast) {
    toast.action?.run();
    ui.dismiss(toast.id);
  }
</script>

{#if ui.toasts.length > 0}
  <div class="stack" aria-live="polite">
    {#each ui.toasts as toast (toast.id)}
      <div class="toast" data-tone={toast.tone} role={toast.tone === 'fail' ? 'alert' : 'status'}>
        <span class="text">{toast.text}</span>
        {#if toast.action}
          <button class="act" type="button" onclick={() => act(toast)}>
            {toast.action.label}
          </button>
        {/if}
        <button class="close btn icon sm ghost" type="button" aria-label="Dismiss" title="Dismiss" onclick={() => ui.dismiss(toast.id)}>
          <X />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .stack {
    position: fixed;
    right: var(--s-4);
    bottom: var(--s-4);
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    gap: var(--s-2);
    width: min(360px, calc(100vw - var(--s-5)));
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: var(--s-2);
    padding: 8px 8px 8px 10px;
    border-radius: var(--r-2);
    background: var(--raised);
    box-shadow:
      inset 2px 0 0 var(--tone-line),
      var(--shadow-pop);
    animation: slide var(--t-med) var(--ease);
  }
  .text {
    flex: 1 1 auto;
    padding-top: 2px;
    font-size: var(--fs-12);
    color: var(--ink);
    text-wrap: pretty;
  }
  .act {
    flex: none;
    height: 20px;
    padding: 0 7px;
    border: 0;
    border-radius: var(--r-1);
    background: var(--tone-wash);
    box-shadow: inset 0 0 0 1px var(--tone-line);
    color: var(--tone, var(--ink-2));
    font-size: var(--fs-11);
    font-weight: 500;
    line-height: 1;
    white-space: nowrap;
  }
  .act:hover {
    background: var(--hover);
    color: var(--ink);
  }
  .close {
    flex: none;
    color: var(--ink-4);
  }
  .close:hover {
    color: var(--ink-2);
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateX(10px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
</style>