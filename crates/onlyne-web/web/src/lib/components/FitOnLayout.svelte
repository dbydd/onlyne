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

  // The key already fitted, so a fit happens once per applied layout.
  //
  // This was a loop, and the depth limit is what it looked like from outside:
  // `fitView` moves the viewport, moving the viewport re-measures the nodes,
  // re-measuring flips `nodesInitialized`, and the effect below — which waits for
  // that flag — called `fitView` again. Recording the key *before* the call ends
  // it: the re-run this provokes finds the layout already fitted and returns.
  let fitted = $state(-1);

  $effect(() => {
    if (fitKey === 0 || fitKey === fitted || !nodesInitialized.current) {
      return;
    }
    fitted = fitKey;
    void fitView({ padding: 0.2, minZoom: 0.2, maxZoom: 2 });
  });
</script>
