<script lang="ts">
  // One thin line per pair of a family's neighbouring cards: the string the
  // plan draws across boards, and the highlight a selection adds to the
  // whole chain. Lines are computed from the DOM after each render, so a
  // card that moved because the stream moved it drags its line with it.
  import { app } from '../store.svelte';

  let { container }: { container: HTMLElement } = $props();

  interface Segment {
    x1: number;
    y1: number;
    x2: number;
    y2: number;
    family: string;
  }

  let segments = $state<Segment[]>([]);
  let size = $state({ w: 0, h: 0 });

  $effect(() => {
    // Reading state here re-runs the effect whenever the boards, the cards,
    // or the selection move.
    app.boards;
    app.selectedFamily;
    const frame = requestAnimationFrame(() => draw(container));
    return () => cancelAnimationFrame(frame);
  });

  function draw(container: HTMLElement) {
    const rect = container.getBoundingClientRect();
    const byFamily = new Map<string, Array<{ hop: number | null; el: HTMLElement }>>();
    for (const el of container.querySelectorAll<HTMLElement>('[data-family]')) {
      const family = el.dataset.family;
      if (!family) continue;
      const hop = el.dataset.hop ? Number(el.dataset.hop) : null;
      const list = byFamily.get(family) ?? [];
      list.push({ hop, el });
      byFamily.set(family, list);
    }
    const drawn: Segment[] = [];
    for (const [family, cards] of byFamily) {
      cards.sort((a, b) => (a.hop ?? 0) - (b.hop ?? 0));
      for (let index = 1; index < cards.length; index += 1) {
        const from = center(cards[index - 1].el, rect);
        const to = center(cards[index].el, rect);
        drawn.push({ ...from, x2: to.x, y2: to.y, family });
      }
    }
    segments = drawn;
    size = { w: container.scrollWidth, h: container.scrollHeight };
  }

  function center(el: HTMLElement, container: DOMRect) {
    const rect = el.getBoundingClientRect();
    return {
      x: rect.left + rect.width / 2 - container.left,
      y: rect.top + rect.height / 2 - container.top,
    };
  }
</script>

<svg class="families" width={size.w} height={size.h} aria-hidden="true">
  {#each segments as segment (segment.family + segment.x1 + segment.y1)}
    <line
      x1={segment.x1}
      y1={segment.y1}
      x2={segment.x2}
      y2={segment.y2}
      class:chain={app.selectedFamily === segment.family}
    />
  {/each}
</svg>

<style>
  svg.families {
    position: absolute;
    inset: 0;
    pointer-events: none;
    z-index: 1;
  }
  line {
    stroke: color-mix(in oklab, currentColor 30%, transparent);
    stroke-width: 1;
  }
  line.chain {
    stroke: var(--accent, #4c8dff);
    stroke-width: 2;
  }
</style>
