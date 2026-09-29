// Where a board sits: the display file's saved places, elkjs's layered start
// when a board has none, and the density rule that turns a near-complete
// graph back into the plain grid of boards (`docs/v2-PLAN.md` lines 388-389).

// The bundled build, not the package entry: `elkjs`'s default entry reaches for
// a bare `web-worker` specifier that a browser cannot resolve, so the built
// bundle died on load and the page came up blank. `elk.bundled.js` carries its
// worker inside the file and needs no such specifier, which is the only reason
// the graph lays out at all here.
import ELK from 'elkjs/lib/elk.bundled.js';
import type { NodePos } from './api';
import type { Board } from '../gen/Board';

/// One allowed route, as the graph draws it.
export interface RouteEdge {
  source: string;
  target: string;
}

/// Cross-role routes the boards declare, with wildcards and names that are
/// no board left out: an edge to nowhere is not a route the canvas can draw.
export function routesOf(boards: Board[]): RouteEdge[] {
  const names = new Set(boards.map((board) => board.role));
  const edges: RouteEdge[] = [];
  for (const board of boards) {
    for (const target of board.edges ?? []) {
      if (target === '*' || !names.has(target)) continue;
      edges.push({ source: board.role, target });
    }
  }
  return edges;
}

/// Past this share of the possible cross pairs — or this absolute count —
/// the edges hide and the boards become the grid, which is the same view the
/// fully-connected case is.
export const DENSITY_SHARE = 0.5;
export const DENSITY_ABSOLUTE = 64;

export function degraded(boards: Board[]): boolean {
  const edges = routesOf(boards).length;
  if (edges > DENSITY_ABSOLUTE) return true;
  const pairs = boards.length * (boards.length - 1);
  return pairs > 0 && edges / pairs > DENSITY_SHARE;
}

const NODE_W = 260;
const NODE_H = 210;

/// The layered start elkjs gives a graph whose boards have no saved place,
/// because most routes have a direction (`docs/v2-PLAN.md` line 388).
export async function layeredStart(
  boards: Board[],
  edges: RouteEdge[],
): Promise<Record<string, NodePos>> {
  const places: Record<string, NodePos> = {};
  try {
    const elk = new ELK();
    const laid = await elk.layout({
      id: 'root',
      layoutOptions: { 'elk.algorithm': 'layered', 'elk.direction': 'RIGHT' },
      children: boards.map((board) => ({ id: board.role, width: NODE_W, height: NODE_H })),
      edges: edges.map((edge, index) => ({
        id: `e${index}`,
        sources: [edge.source],
        targets: [edge.target],
      })),
    });
    for (const child of laid.children ?? []) {
      places[child.id] = { x: child.x ?? 0, y: child.y ?? 0 };
    }
  } catch {
    // elkjs failing to place a graph is not a reason to draw nothing: the
    // deterministic grid below stands in, and dragging still works.
  }
  for (const [index, board] of boards.entries()) {
    if (!places[board.role]) {
      places[board.role] = gridPlace(index);
    }
  }
  return places;
}

/// The plain boards' arrangement: columns of three, the view the dense graph
/// degrades to and the grid mode always is.
export function gridPlaces(boards: Board[]): Record<string, NodePos> {
  const places: Record<string, NodePos> = {};
  for (const [index, board] of boards.entries()) {
    places[board.role] = gridPlace(index);
  }
  return places;
}

function gridPlace(index: number): NodePos {
  const perRow = 3;
  return {
    x: (index % perRow) * (NODE_W + 40),
    y: Math.floor(index / perRow) * (NODE_H + 40),
  };
}
