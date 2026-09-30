// Where a board sits: the browser's own memory of where it was put, the
// layered start for a board that has none, and the density rule that dims the
// edges of a near-complete graph instead of replacing it.
//
// The layout is **session state, not a document**. Nothing about where an
// operator dragged a board belongs to the cluster: the spec holds semantics and
// stays free of coordinates, and a file beside the spec bought a portability
// nobody asked for at the price of a write path that raced the browser and
// could lose a drag while answering `ok`.
//
// `sessionStorage` rather than `localStorage` because the requirement is the
// current session and nothing more. It is per tab, and this surface's origin
// carries a port that changes on every launch, so a fresh `onlyne-web` is a
// fresh slate without anything having to clear it. Every read and write is
// guarded, because a browser with storage disabled must still lay out a graph —
// the automatic layout is a working answer, not a degraded one.
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
  const named = new Set(boards.map((board) => board.role));
  const edges: RouteEdge[] = [];
  for (const board of boards) {
    for (const target of board.edges ?? []) {
      if (target === '*' || !named.has(target)) continue;
      if (target === board.role) continue;
      edges.push({ source: board.role, target });
    }
  }
  return edges;
}

/// When the edges dim.
///
/// A share alone reads a two-role cluster as dense: both roles route to each
/// other, so the drawn edges are twice the possible pairs and every small graph
/// would announce itself crowded. The share is only read over a graph big enough
/// for "most pairs" to mean something, and the absolute count carries the rest —
/// which is the case the share was standing in for anyway.
export const DENSITY_SHARE = 0.5;
export const DENSITY_ROLES = 4;
export const DENSITY_ABSOLUTE = 64;

export function degraded(boards: Board[]): boolean {
  const roles = boards.length;
  const drawn = routesOf(boards).length;
  if (drawn >= DENSITY_ABSOLUTE) return true;
  if (roles < DENSITY_ROLES) return false;
  const possible = (roles * (roles - 1)) / 2;
  return possible > 0 && drawn / possible >= DENSITY_SHARE;
}

const NODE_W = 260;
const NODE_H = 210;

/// The layered start elkjs gives a graph whose boards have no place, because
/// most routes have a direction (`docs/v2-PLAN.md` line 388).
export async function layeredStart(
  boards: Board[],
  edges: RouteEdge[],
): Promise<Record<string, NodePos>> {
  const graph = {
    id: 'root',
    layoutOptions: {
      'elk.algorithm': 'layered',
      'elk.direction': 'RIGHT',
      'elk.layered.spacing.nodeNodeBetweenLayers': '80',
      'elk.spacing.nodeNode': '40',
    },
    children: boards.map((board) => ({
      id: board.role,
      width: NODE_W,
      height: NODE_H,
    })),
    edges: edges.map((edge) => ({ id: `${edge.source}->${edge.target}`, sources: [edge.source], targets: [edge.target] })),
  };
  const elk = new ELK();
  // elkjs types its input as a shape this graph is a member of, and the cast
  // takes the *result* to `never` with it; the result is read through its own
  // shape instead, which is all this function uses.
  const laid = (await elk.layout(graph as never)) as {
    children?: Array<{ id: string; x?: number; y?: number }>;
  };
  const places: Record<string, NodePos> = {};
  for (const child of laid.children ?? []) {
    places[child.id] = { x: child.x ?? 0, y: child.y ?? 0 };
  }
  return places;
}

const KEY = 'onlyne.layout';

/// This tab's places, or `{}` when there are none or storage is unavailable.
export function readPlaces(): Record<string, NodePos> {
  try {
    const raw = sessionStorage.getItem(KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, NodePos>;
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    // A browser with storage disabled still lays out a graph; the automatic
    // layout is the whole answer there, not a reduced one.
    return {};
  }
}

/// Remember where the boards are, for as long as this tab is open.
export function writePlaces(places: Record<string, NodePos>): void {
  try {
    sessionStorage.setItem(KEY, JSON.stringify(places));
  } catch {
    // A full or disabled store costs the layout its next reload and nothing else.
  }
}
