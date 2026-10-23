<script lang="ts">
  import { BaseEdge, EdgeLabel, getBezierPath } from '@xyflow/svelte';
  import type { Edge, EdgeProps } from '@xyflow/svelte';
  import type { Tone } from '../lib/model';

  type TraceData = { steps: number[]; tone: Tone };
  type TraceGraphEdge = Edge<TraceData, 'trace'>;

  let {
    id,
    source,
    target,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    data,
  }: EdgeProps<TraceGraphEdge> = $props();

  let [path, labelX, labelY] = $derived(
    getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition }),
  );
  let steps = $derived(data?.steps ?? []);
  let tone = $derived(data?.tone ?? 'plain');
</script>

<BaseEdge
  {id}
  {path}
  class="trace-edge"
  interactionWidth={0}
  aria-label={`${source} to ${target}`}
/>
<EdgeLabel x={labelX} y={labelY} class="trace-label">
  <span class="trace-steps mono" data-tone={tone}>{steps.join(' · ')}</span>
</EdgeLabel>
