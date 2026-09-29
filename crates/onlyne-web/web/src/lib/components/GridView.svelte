<script lang="ts">
  // The plain boards: a grid of boards, the family lines strung across them,
  // and nothing else. This is the view the dense graph degrades to, which is
  // the same view the fully-connected case is (`docs/v2-PLAN.md` line 389).
  import Board from './Board.svelte';
  import FamilyLines from './FamilyLines.svelte';
  import { app } from '../store.svelte';

  let pane = $state<HTMLElement | null>(null);
</script>

<div class="grid" bind:this={pane}>
  {#if pane}
    <FamilyLines container={pane} />
  {/if}
  {#each app.boards as board (board.role)}
    <Board {board} />
  {/each}
</div>

<style>
  .grid {
    position: relative;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(560px, 1fr));
    gap: 16px;
    align-items: start;
    padding: 16px;
  }
</style>
