<script lang="ts">
  // The fit, once the layout has put the boards where they belong.
  //
  // It is a child of `<SvelteFlow>` because the flow's store only reaches a
  // component through the context `SvelteFlow` sets for its children — the
  // parent holds the nodes and has no handle on the viewport.
  //
  // The fit is asked for by `fitKey`, which the graph bumps once a layout has
  // been *applied* — not when it resolves. The two are not the same moment:
  // the positions a layout resolves are not on the nodes until they are
  // written, and a fit that runs before that measures every board still
  // sitting at the origin, which is a viewport framing one corner of an
  // empty graph.
  //
  // `nodesInitialized` is the other half of the wait: a board's bounds only
  // exist once the canvas has measured it, and a fit over unmeasured nodes
  // frames nothing. Both are read here, so the effect runs again for whichever
  // arrives last.
  import { useNodesInitialized, useSvelteFlow } from '@xyflow/svelte';

  let { fitKey }: { fitKey: number } = $props();

  const { fitView } = useSvelteFlow();
  const nodesInitialized = useNodesInitialized();

  $effect(() => {
    if (fitKey === 0 || !nodesInitialized.current) {
      return;
    }
    void fitView({ padding: 0.2, minZoom: 0.2, maxZoom: 2 });
  });
</script>
