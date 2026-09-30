<script lang="ts">
  // The route graph: boards as nodes, allowed routes as edges, zoom and pan
  // and drag as Svelte Flow gives them. Dragging a board saves its place to
  // the display file; dragging a line between handles applies a typed
  // `set_targets` edit — which is why a second browser sees the new route
  // after the reload, not because anything was drawn locally.
  import { SvelteFlow } from '@xyflow/svelte';
  import type { Connection, Edge, Node, NodeChange } from '@xyflow/svelte';
  import BoardNode from './BoardNode.svelte';
  import FitOnLayout from './FitOnLayout.svelte';
  import { addRoute, app } from '../store.svelte';
  import { getLayout, putLayout } from '../api';
  import { layeredStart, routesOf } from '../layout';

  const NODE_W = 240;
  const NODE_H = 190;

  const nodeTypes = { board: BoardNode };

  let nodes = $state<Node[]>([]);
  let edges = $state<Edge[]>([]);
  let saved = $state<Record<string, { x: number; y: number }>>({});
  // Bumped once per applied layout; `FitOnLayout` fits the viewport when it
  // changes. It counts *applied* layouts, not resolved ones: a fit taken
  // before the positions are on the nodes measures a graph still stacked at
  // the origin.
  let fitKey = $state(0);
  let placed = '';

  $effect(() => {
    const boards = app.boards;
    const routes = routesOf(boards);
    edges = routes.map((route) => ({
      id: `${route.source}->${route.target}`,
      source: route.source,
      target: route.target,
    }));
    sync(boards, saved);
    // A board arriving or leaving is a re-layout: the graph is drawn again
    // and the viewport has to frame what is now there.
    const roles = boards.map((board) => board.role).join(',');
    if (roles !== placed) {
      placed = roles;
      fitKey += 1;
    }
    // Boards with no saved place get the layered start once.
    const missing = boards.filter((board) => !saved[board.role]);
    if (missing.length > 0) {
      layeredStart(boards, routes).then((places) => {
        saved = { ...saved, ...places };
        sync(boards, saved);
        // Resolving is not placing: the places are on the nodes now, so this
        // is the first moment a fit sees anything but the origin.
        fitKey += 1;
      });
    }
  });

  function sync(
    boards: Array<{ role: string }>,
    places: Record<string, { x: number; y: number }>,
  ) {
    // Built in one pass, from `boards` and never from `nodes`. Writing `nodes`
    // and then reading it back made this effect depend on the state it writes,
    // so it re-ran until Svelte gave up with `effect_update_depth_exceeded` and
    // the graph pegged the main thread. A node's data carries its board, matched
    // here by id so a board that moved still renders the board it is.
    nodes = boards.map((board) => ({
      id: board.role,
      type: 'board',
      position: places[board.role] ?? { x: 0, y: 0 },
      data: { board: app.boards.find((candidate) => candidate.role === board.role) },
      width: NODE_W,
      height: NODE_H,
    }));
  }

  function onNodesChange(changes: NodeChange[]) {
    for (const change of changes) {
      if (change.type === 'position' && change.position) {
        const node = nodes.find((candidate) => candidate.id === change.id);
        if (node) {
          node.position = { ...change.position };
        }
      }
      if (change.type === 'remove') {
        nodes = nodes.filter((candidate) => candidate.id !== change.id);
      }
    }
  }

  function onNodeDragStop(_event: unknown, node: Node) {
    saved = { ...saved, [node.id]: { ...node.position } };
    putLayout(app.token, saved);
  }

  function onConnect(connection: Connection) {
    if (connection.source && connection.target && connection.source !== connection.target) {
      addRoute(connection.source, connection.target);
    }
  }

  getLayout(app.token).then((layout) => {
    saved = layout.nodes;
    sync(app.boards, saved);
    // The saved places are on the nodes, so the graph can be framed.
    fitKey += 1;
  });
</script>

<SvelteFlow
  {nodes}
  {edges}
  {nodeTypes}
  {onNodesChange}
  {onConnect}
  onnodedragstop={onNodeDragStop}
  minzoom={0.2}
  maxzoom={2}
  connectradius={26}
>
  <FitOnLayout {fitKey} />
</SvelteFlow>

<style>
  :global(.svelte-flow) {
    height: 100%;
  }
</style>
