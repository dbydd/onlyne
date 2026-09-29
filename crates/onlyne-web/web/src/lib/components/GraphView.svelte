<script lang="ts">
  // The route graph: boards as nodes, allowed routes as edges, zoom and pan
  // and drag as Svelte Flow gives them. Dragging a board saves its place to
  // the display file; dragging a line between handles applies a typed
  // `set_targets` edit — which is why a second browser sees the new route
  // after the reload, not because anything was drawn locally.
  import { SvelteFlow } from '@xyflow/svelte';
  import type { Connection, Edge, Node, NodeChange } from '@xyflow/svelte';
  import BoardNode from './BoardNode.svelte';
  import { addRoute, app } from '../store.svelte';
  import { getLayout, putLayout } from '../api';
  import { layeredStart, routesOf } from '../layout';

  const NODE_W = 240;
  const NODE_H = 190;

  const nodeTypes = { board: BoardNode };

  let nodes = $state<Node[]>([]);
  let edges = $state<Edge[]>([]);
  let saved = $state<Record<string, { x: number; y: number }>>({});

  $effect(() => {
    const boards = app.boards;
    const routes = routesOf(boards);
    edges = routes.map((route) => ({
      id: `${route.source}->${route.target}`,
      source: route.source,
      target: route.target,
    }));
    sync(boards, saved);
    // Boards with no saved place get the layered start once.
    const missing = boards.filter((board) => !saved[board.role]);
    if (missing.length > 0) {
      layeredStart(boards, routes).then((places) => {
        saved = { ...saved, ...places };
        sync(boards, saved);
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
  });
</script>

<SvelteFlow
  {nodes}
  {edges}
  {nodeTypes}
  {onNodesChange}
  {onConnect}
  onnodedragstop={onNodeDragStop}
  fitViewAfterInit
  minzoom={0.2}
  maxzoom={2}
  connectradius={26}
/>

<style>
  :global(.svelte-flow) {
    height: 100%;
  }
</style>
