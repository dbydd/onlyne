<script lang="ts">
  // The fit, asked for by a number rather than by a state object.
  //
  // It is a child of `<SvelteFlow>` because the viewport only reaches a
  // component through the context the flow sets for its children; the parent
  // owns the nodes and has no handle on the camera.
  //
  // Two waits, and both are load-bearing. The fit is asked for once a layout
  // has been *applied* — the positions a layout resolves are not on the nodes
  // until they are written, and a fit that runs before that frames every board
  // still stacked at the origin. And a board has no bounds until the canvas has
  // measured it, so `useNodesInitialized()` is the other half.
  //
  // `fitted` records the request *before* the call. A fit moves the viewport,
  // which re-measures the nodes, which flips the flag this effect waits on —
  // and without that record the effect calls `fitView` again, forever.
  import { useNodesInitialized, useSvelteFlow } from '@xyflow/svelte';

  interface Props {
    /// Bumped by the canvas when the viewport should be reframed.
    fitKey: number;
    /// The roles to frame, or empty for the whole graph.
    roles?: string[];
  }

  let { fitKey, roles = [] }: Props = $props();

  const { fitView } = useSvelteFlow();
  const initialized = useNodesInitialized();

  let fitted = $state(-1);

  $effect(() => {
    if (fitKey === 0 || fitKey === fitted || !initialized.current) return;
    fitted = fitKey;
    const nodes = roles.length > 0 ? roles.map((id) => ({ id })) : undefined;
    void fitView({ nodes, padding: 0.25, minZoom: 0.25, maxZoom: 1.1, duration: 240 });
  });
</script>
