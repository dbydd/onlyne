<script lang="ts">
  // A relative time, from the one shared tick. Nothing here starts a timer.
  import { HHMMSS, span } from '../format';
  import { tick } from '../state/clock.svelte';

  interface Props {
    at: number;
  }

  let { at }: Props = $props();
</script>

<time class="ago" title={at > 0 ? HHMMSS(at) : ''}>{at > 0 ? span(tick.now - at) : ''}</time>

<style>
  .ago {
    font-family: var(--font-mono);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    color: var(--ink-4);
  }
</style>
