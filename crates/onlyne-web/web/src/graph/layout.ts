import ELK from 'elkjs/lib/elk.bundled.js';
import { NODE_H, NODE_W, type GraphPosition } from './geometry';

interface LayoutRoute {
  id?: string;
  source: string;
  target: string;
}

const elk = new ELK();

/** Arrange boards in a rightward layered graph without touching their saved places. */
export async function layoutRoles(roles: string[], routes: LayoutRoute[]): Promise<Record<string, GraphPosition>> {
  if (roles.length === 0) return {};

  const known = new Set(roles);
  const graph = {
    id: 'onlyne-graph',
    layoutOptions: {
      'elk.algorithm': 'layered',
      'elk.direction': 'RIGHT',
      'elk.spacing.nodeNode': '48',
      'elk.layered.spacing.nodeNodeBetweenLayers': '88',
      'elk.layered.nodePlacement.strategy': 'NETWORK_SIMPLEX',
    },
    children: roles.map((role) => ({ id: role, width: NODE_W, height: NODE_H })),
    edges: routes
      .filter((route) => known.has(route.source) && known.has(route.target) && route.source !== route.target)
      .map((route, index) => ({
        id: route.id ?? `${route.source}->${route.target}-${index}`,
        sources: [route.source],
        targets: [route.target],
      })),
  };

  const result = await elk.layout(graph);
  const positions: Record<string, GraphPosition> = {};
  for (const node of result.children ?? []) {
    positions[node.id] = { x: node.x ?? 0, y: node.y ?? 0 };
  }
  return positions;
}
