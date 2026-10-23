<script lang="ts">
  // The canvas: a board per role, a line per declared route, and the trace of
  // one task family over both.
  //
  // Three things here are load-bearing, and each one cost something to learn.
  //
  // **Node objects must be reference-stable across frames.** A stream frame
  // carries the whole view, so a node whose `data` held the board object would
  // hand Svelte Flow a new object graph on every heartbeat and it would re-adopt
  // every node every time. So `data` carries the role and nothing else, and
  // `RoleNode` looks its own board up. The node *array* is keyed on the roles
  // and the places, which is the only pair that can move a board.
  //
  // **The effect below reads the store and writes `places`**, never the other
  // way round. An effect that read its own output would re-run until Svelte
  // abandons the tree.
  //
  // **A drag reports itself once, on stop.** Svelte Flow moves the board
  // internally while the pointer is down; the place is written when it is let
  // go, and the node array is rebuilt from that place.
  import { Background, BackgroundVariant, Controls, MarkerType, SvelteFlow } from '@xyflow/svelte';
  import type { Connection, Edge, Node } from '@xyflow/svelte';
  import ArrowsOutSimple from 'phosphor-svelte/lib/ArrowsOutSimple';
  import ArrowCounterClockwise from 'phosphor-svelte/lib/ArrowCounterClockwise';
  import Graph from 'phosphor-svelte/lib/Graph';
  import Plus from 'phosphor-svelte/lib/Plus';
  import { addRoute, removeRoute } from '../lib/ops';
  import { clip } from '../lib/format';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';
  import Empty from '../lib/ui/Empty.svelte';
  import FitOnLayout from './FitOnLayout.svelte';
  import { layoutRoles } from './layout';
  import { places } from './places.svelte';
  import RoleNode from './RoleNode.svelte';
  import RouteEdge from './RouteEdge.svelte';
  import TraceEdge from './TraceEdge.svelte';
  import './graph.css';

  const nodeTypes = { board: RoleNode };
  const edgeTypes = { route: RouteEdge, trace: TraceEdge };

  /// The roles the canvas draws, as one string. A derived value over a joined
  /// key is how a list of primitives stays comparable: the frame that only
  /// moves a delivery leaves this string alone, and nothing downstream of it
  /// re-runs.
  const roleKey = $derived(cluster.canvasBoards.map((board) => board.role).join('\u0000'));

  const nodes = $derived.by<Node[]>(() => {
    const roles = roleKey === '' ? [] : roleKey.split('\u0000');
    return roles.map((role) => ({
      id: role,
      type: 'board',
      position: placesFor(role),
      // Reference-stable by construction: `data` holds the role, and the role is
      // this node's id — so two frames that differ only in what a board is doing
      // hand Svelte Flow the very same node object shape it already adopted.
      data: { role },
      draggable: true,
      deletable: false,
      connectable: true,
    }));
  });

  /// The drawn routes, plus the trace of the selected family over them. A trace
  /// edge is drawn even where it runs against the declared routes: a completion
  /// reports home, and that leg is the end of the story.
  const edges = $derived.by<Edge[]>(() => {
    const drawn: Edge[] = cluster.routes.map((route) => ({
      id: `route:${route.id}`,
      type: 'route',
      source: route.source,
      target: route.target,
      deletable: true,
      selectable: true,
      // The arrow colour comes from `graph.css` through `--xy-edge-stroke`;
      // an inline style object is a type error in this version, and this is why.
      markerEnd: { type: MarkerType.ArrowClosed },
    }));
    const trace = ui.trace;
    if (!trace) return drawn;
    const canvas = cluster.canvasRoles;
    const traced: Edge[] = trace.edges
      .filter((edge) => canvas.has(edge.source) && canvas.has(edge.target))
      .map((edge) => ({
        id: `trace:${edge.id}`,
        type: 'trace',
        source: edge.source,
        target: edge.target,
        data: { steps: edge.steps.map((step) => step + 1), tone: edge.tone },
        deletable: false,
        selectable: false,
        focusable: false,
        zIndex: 10,
      }));
    return [...drawn, ...traced];
  });

  /// A board's place, or the origin: `elk` fills the places in, and a board that
  /// has none yet is placed by the layout pass below.
  function placesFor(role: string): { x: number; y: number } {
    return places.positions[role] ?? { x: 0, y: 0 };
  }

  /// Bumped when the viewport should reframe: once after a layout lands, and
  /// again whenever the operator asks for something to be brought into view.
  let fitKey = $state(0);
  let fitRoles = $state<string[]>([]);
  /// The role set the last layout was computed for, so a stream of frames does
  /// not start a layout each time.
  let laidOutFor = '';
  let laying = false;

  $effect(() => {
    const roles = roleKey === '' ? [] : roleKey.split('\u0000');
    void cluster.routes.length;
    // A board that has no place gets one, and only those. `layoutRoles` answers
    // for every role it is handed, so taking its whole answer would send every
    // dragged board back to the automatic position the moment a role joined.
    const missing = places.missingOf(roles);
    if (missing.length === 0) {
      if (roles.join(',') !== laidOutFor) {
        laidOutFor = roles.join(',');
        fitRoles = [];
        fitKey += 1;
      }
      return;
    }
    if (laying) return;
    laying = true;
    void layoutRoles(roles, cluster.routes).then((positions) => {
      laying = false;
      let moved = false;
      for (const role of missing) {
        // Asked again here rather than remembered from when the question went
        // out: a layout takes long enough for the operator to have dragged that
        // very board, and a place captured before the drag would overwrite it.
        if (places.positions[role]) continue;
        const position = positions[role];
        if (!position) continue;
        places.place(role, position);
        moved = true;
      }
      laidOutFor = roles.join(',');
      fitRoles = [];
      if (moved || roles.length > 0) fitKey += 1;
    });
  });

  /// A selection made from a list (the palette, the ledger, a fault) asks for
  /// its boards to be brought into view. A click on the canvas does not: moving
  /// the camera under a click that was meant to pick something is its own kind
  /// of wrong.
  let revealed = -1;
  $effect(() => {
    if (ui.revealSeq === revealed) return;
    revealed = ui.revealSeq;
    fitRoles = ui.revealRoles.filter((role) => cluster.canvasRoles.has(role));
    fitKey += 1;
  });

  /// Reset: forget every place, lay out again, reframe.
  let reset = -1;
  $effect(() => {
    if (ui.layoutSeq === reset) return;
    reset = ui.layoutSeq;
    places.clear();
    laidOutFor = '';
    void layoutRoles(
      roleKey === '' ? [] : roleKey.split('\u0000'),
      cluster.routes,
    ).then((positions) => {
      const roles = roleKey === '' ? [] : roleKey.split('\u0000');
      for (const role of roles) {
        const position = positions[role];
        if (position) places.place(role, position);
      }
      fitRoles = [];
      fitKey += 1;
    });
  });

  function onConnect(connection: Connection) {
    if (connection.source && connection.target && connection.source !== connection.target) {
      void addRoute(connection.source, connection.target);
    }
  }

  /// Withdrawing is the line read the other way: the same typed edit that
  /// declared it. Right-click, or select it and press delete.
  function onDelete({ nodes: gone, edges: removed }: { nodes: Node[]; edges: Edge[] }) {
    void gone;
    for (const edge of removed) {
      const route = cluster.routes.find((candidate) => `route:${candidate.id}` === edge.id);
      if (route) void removeRoute(route.source, route.target);
    }
  }

  function onEdgeClick({ edge }: { edge: Edge }) {
    const route = cluster.routes.find((candidate) => `route:${candidate.id}` === edge.id);
    if (route) ui.select({ kind: 'route', source: route.source, target: route.target }, false);
  }
</script>

{#if cluster.canvasBoards.length === 0}
  <Empty
    icon={Graph}
    title="No roles yet"
    hint="Roles are declared in the cluster's spec.toml. Declare one here and its board appears; its client runs on the machine that owns its workspace."
  >
    <button class="btn primary" onclick={() => ui.select({ kind: 'declare' })}>
      <Plus size="12" weight="bold" /> Declare a role
    </button>
  </Empty>
{:else}
  <div class="canvas">
    <div class="canvas-controls">
      <button
        class="btn icon sm ghost"
        title="Fit the whole graph"
        aria-label="Fit the whole graph"
        onclick={() => {
          fitRoles = [];
          fitKey += 1;
        }}
      >
        <ArrowsOutSimple size="13" />
      </button>
      <button
        class="btn icon sm ghost"
        title="Lay the boards out again"
        aria-label="Lay the boards out again"
        onclick={() => ui.resetLayout()}
      >
        <ArrowCounterClockwise size="13" />
      </button>
      <button
        class="btn icon sm ghost"
        title="Declare a role"
        aria-label="Declare a role"
        onclick={() => ui.select({ kind: 'declare' })}
      >
        <Plus size="13" weight="bold" />
      </button>
    </div>

    <SvelteFlow
      {nodes}
      {edges}
      {nodeTypes}
      {edgeTypes}
      onconnect={onConnect}
      ondelete={onDelete}
      onedgeclick={onEdgeClick}
      onedgecontextmenu={({ edge }) => onEdgeClick({ edge })}
      onpaneclick={() => ui.clear()}
      minZoom={0.2}
      maxZoom={2}
      connectionRadius={26}
      panOnScroll={false}
      zoomOnScroll={true}
      nodesConnectable={true}
      colorMode="dark"
    >
      <Background variant={BackgroundVariant.Dots} gap={22} size={1.4} />
      <Controls />
      <FitOnLayout {fitKey} roles={fitRoles} />
    </SvelteFlow>
    <span class="sr-only" aria-live="polite">
      {clip(`${cluster.canvasBoards.length} boards and ${cluster.routes.length} routes drawn`, 80)}
    </span>
  </div>
{/if}

<style>
  .canvas {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 0;
  }
  .canvas-controls {
    position: absolute;
    top: var(--s-3);
    left: var(--s-3);
    z-index: 4;
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r-2);
    background: var(--panel);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .canvas :global(.svelte-flow) {
    height: 100%;
  }
</style>
