<script lang="ts">
  // The route graph: one role per board, the allowed routes drawn between them,
  // and a drag that says where a board sits.
  //
  // Two things here are load-bearing and both were learned the hard way.
  //
  // **The effect below reads `app.boards` and nothing else.** It also writes
  // `nodes`, `edges` and `fitKey`, so reading any of those back would make it
  // depend on its own output and re-run until Svelte abandons the tree with
  // `effect_update_depth_exceeded` — at which point the edges, built in the same
  // pass, never render and the whole surface comes up blank. That is why the
  // places live in a plain object rather than in `$state`: nothing here tracks
  // them, and the handlers that do write them re-sync by hand.
  //
  // **A drag reads one object, not two arguments.** Svelte Flow hands the
  // drag-stop a `{ event, targetNode, nodes }` payload; reading the node out of a
  // second parameter found `undefined` there, every drag threw on the way to the
  // write, and the board moved perfectly while the layout was silently lost.
  import { onMount } from 'svelte';
  import { MarkerType, SvelteFlow } from '@xyflow/svelte';
  import type { Connection, Edge, Node, NodeChange } from '@xyflow/svelte';
  import BoardNode from './BoardNode.svelte';
  import FitOnLayout from './FitOnLayout.svelte';
  import { addRoute, app } from '../store.svelte';
  import { degraded, layeredStart, readPlaces, routesOf, writePlaces } from '../layout';

  const NODE_W = 240;
  const NODE_H = 190;

  const nodeTypes = { board: BoardNode };

  let nodes = $state<Node[]>([]);
  let edges = $state<Edge[]>([]);
  /// Where each board sits. Deliberately not reactive — see the note above.
  const saved: Record<string, { x: number; y: number }> = {};
  /// Bumped once per applied layout; `FitOnLayout` fits the viewport when it
  /// changes. It counts *applied* layouts, not resolved ones: a fit taken before
  /// the positions are on the nodes measures a graph still stacked at the origin.
  let fitKey = $state(0);
  /// The role set last fitted, so a board arriving or leaving frames the graph
  /// again and a stream of events with the same roles does not.
  let placed = '';
  /// The auto-layout is in flight, so a second event arriving mid-resolve does
  /// not start a second one over the same boards.
  let laying = false;

  $effect(() => {
    const boards = app.boards;
    const routes = routesOf(boards);
    // The stroke and the arrowhead are named rather than inherited. A line whose
    // colour comes from a stylesheet nobody wrote is a line nobody has seen, and
    // these are the edges this whole surface exists to draw.
    //
    // They dim when the graph is crowded, which is what the header's notice says
    // they do — a notice naming a treatment the canvas did not apply was its own
    // small lie.
    const faint = degraded(boards);
    const stroke = faint ? 'rgba(150,160,180,0.28)' : 'rgba(150,175,220,0.75)';
    edges = routes.map((route) => ({
      id: `${route.source}->${route.target}`,
      source: route.source,
      target: route.target,
      type: 'default',
      style: { stroke },
      markerEnd: { type: MarkerType.ArrowClosed, color: stroke, width: 14, height: 14 },
    }));
    sync(boards);
    const roles = boards.map((board) => board.role).join(',');
    if (roles !== placed) {
      placed = roles;
      fitKey += 1;
    }
    // A board with no place gets one, and only those. `layeredStart` answers for
    // every board it is handed, so taking its whole answer sent every dragged
    // position back to the automatic one the moment a role joined the cluster.
    if (laying) return;
    const missing = boards.filter((board) => !saved[board.role]);
    if (missing.length === 0) return;
    laying = true;
    void layeredStart(boards, routes).then((places) => {
      laying = false;
      const fresh: Record<string, { x: number; y: number }> = {};
      for (const board of missing) {
        // Asked again at the moment the answer lands, not remembered from when
        // the question went out: a resolve takes long enough for the operator to
        // have dragged that very board, and a place captured before the drag
        // overwrote it after.
        if (saved[board.role]) continue;
        const place = places[board.role];
        if (place) fresh[board.role] = place;
      }
      if (Object.keys(fresh).length === 0) return;
      Object.assign(saved, fresh);
      sync(boards);
      writePlaces(saved);
      // Resolving is not placing: the places are on the nodes now, so this is
      // the first moment a fit sees anything but the origin.
      fitKey += 1;
    });
  });

  /// Rebuild the nodes from the boards and the places, in one pass and never
  /// from `nodes` — writing `nodes` and reading it back is the same self-
  /// dependency the effect above is kept clear of.
  function sync(boards: Array<{ role: string }>) {
    nodes = boards.map((board) => ({
      id: board.role,
      type: 'board',
      position: saved[board.role] ?? { x: 0, y: 0 },
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

  function onNodeDragStop({ targetNode }: { targetNode: Node | null }) {
    if (!targetNode) return;
    saved[targetNode.id] = { ...targetNode.position };
    // The tab's own memory, written when the operator lets go rather than on
    // every frame: a drag emits a position change per pointer move.
    writePlaces(saved);
    sync(app.boards);
  }

  function onConnect(connection: Connection) {
    if (connection.source && connection.target && connection.source !== connection.target) {
      addRoute(connection.source, connection.target);
    }
  }

  // This tab's places, read once on mount. An effect would be the wrong
  // instrument for a read that happens once, and a mount cannot form the cycle
  // the note at the top describes.
  onMount(() => {
    const places = readPlaces();
    if (Object.keys(places).length === 0) return;
    Object.assign(saved, places);
    sync(app.boards);
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
