// Derived in part from graphtatui (c) Sok205 contributors, MIT/Apache-2.0.
//! The page-1 map's geometry, ported from graphtatui's explorer
//! (`src/tui/explorer/{layout,radial,lod}.rs`): a Fruchterman-Reingold force
//! relaxation, hop-distance radial seeding, and the semantic level-of-detail
//! tiers. Pure geometry over points and indices, so it is unit-testable and
//! carries no protocol, terminal, or storage knowledge.
//!
//! Two deliberate departures from upstream:
//!
//! * Repulsion is computed exactly over every pair instead of through a
//!   Barnes-Hut quadtree. Upstream's quadtree only approximates at
//!   `theta > 0`; a role graph is tens of nodes, so the exact pass is the same
//!   force field with a fixed, order-independent summation.
//! * A final separation sweep keeps two role boxes from overlapping, which the
//!   circle-and-Braille renderer upstream never needed.

use std::collections::VecDeque;

const EPS: f32 = 1e-6;

/// Start of the temperature schedule, as a fraction of the ideal edge length:
/// hot enough that a node can cross a good part of one hop per iteration.
const RESTART_TEMP_FRACTION: f32 = 0.3;
/// Grows per iteration; a node's displacement is clamped to it.
const COOLING: f32 = 0.94;
/// The layout is settled once its temperature drops below this.
const SETTLE_EPSILON: f32 = 0.05;
/// Steps a settle may take before it stops regardless (the schedule converges
/// well inside this).
const SETTLE_STEPS: usize = 240;

/// A point, or a vector, in the map's coordinate space. One unit is one
/// terminal cell at zoom 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn distance(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Tunable forces for the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutParams {
    /// Ideal edge length (`k` in Fruchterman-Reingold).
    pub ideal_len: f32,
    /// Repulsion strength multiplier; the map's `+`/`-` keys drive this.
    pub repulsion: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            ideal_len: 10.0,
            repulsion: 1.0,
        }
    }
}

/// Apply one Fruchterman-Reingold iteration.
///
/// `positions` and `pinned` are parallel arrays (one entry per node); `edges`
/// holds index pairs into them. Per-node displacement is clamped to `temp` (the
/// cooling temperature); pinned nodes never move.
pub fn step(
    positions: &mut [Vec2],
    edges: &[(usize, usize)],
    pinned: &[bool],
    temp: f32,
    params: &LayoutParams,
) {
    let n = positions.len();
    let k = params.ideal_len.max(EPS);
    let mut dx = vec![0.0f32; n];
    let mut dy = vec![0.0f32; n];

    // Repulsion between every pair: f_r = repulsion * k^2 / d.
    for a in 0..n {
        for b in a + 1..n {
            let ex = positions[a].x - positions[b].x;
            let ey = positions[a].y - positions[b].y;
            let dist = (ex * ex + ey * ey).sqrt();
            if dist < EPS {
                // Coincident bodies: push them apart on a fixed axis so the
                // next step has a direction to work with.
                let force = params.repulsion * k;
                dx[a] += force;
                dx[b] -= force;
                continue;
            }
            let force = params.repulsion * k * k / dist;
            dx[a] += ex / dist * force;
            dy[a] += ey / dist * force;
            dx[b] -= ex / dist * force;
            dy[b] -= ey / dist * force;
        }
    }

    // Attraction along edges: f_a = d^2 / k.
    for &(a, b) in edges {
        let ex = positions[a].x - positions[b].x;
        let ey = positions[a].y - positions[b].y;
        let dist = (ex * ex + ey * ey).sqrt().max(EPS);
        let force = dist * dist / k;
        let (ux, uy) = (ex / dist, ey / dist);
        dx[a] -= ux * force;
        dy[a] -= uy * force;
        dx[b] += ux * force;
        dy[b] += uy * force;
    }

    // Apply displacement, clamped to the cooling temperature; pinned nodes hold
    // still.
    for i in 0..n {
        if pinned.get(i).copied().unwrap_or(false) {
            continue;
        }
        let len = (dx[i] * dx[i] + dy[i] * dy[i]).sqrt();
        if len > EPS {
            let scale = len.min(temp) / len;
            positions[i].x += dx[i] * scale;
            positions[i].y += dy[i] * scale;
        }
    }
}

/// Relax `positions` to (near) convergence on a cooling schedule, so the first
/// frame draws a settled map rather than the seed.
pub fn settle(
    positions: &mut [Vec2],
    edges: &[(usize, usize)],
    pinned: &[bool],
    params: &LayoutParams,
) {
    let mut temp = (params.ideal_len * RESTART_TEMP_FRACTION).max(4.0);
    for _ in 0..SETTLE_STEPS {
        step(positions, edges, pinned, temp, params);
        temp *= COOLING;
        if temp < SETTLE_EPSILON {
            break;
        }
    }
}

/// Push apart any two nodes whose boxes touch, so the drawn rectangles never
/// overlap. `half_w`/`half_h` are half the box, in layout units; pinned nodes
/// never move.
pub fn separate(positions: &mut [Vec2], half_w: f32, half_h: f32, pinned: &[bool], sweeps: usize) {
    let n = positions.len();
    let min_x = 2.0 * half_w;
    let min_y = 2.0 * half_h;
    for _ in 0..sweeps {
        let mut moved = false;
        for a in 0..n {
            for b in a + 1..n {
                let dx = positions[b].x - positions[a].x;
                let dy = positions[b].y - positions[a].y;
                let overlap_x = min_x - dx.abs();
                let overlap_y = min_y - dy.abs();
                if overlap_x <= 0.0 || overlap_y <= 0.0 {
                    continue;
                }
                let (a_pinned, b_pinned) = (
                    pinned.get(a).copied().unwrap_or(false),
                    pinned.get(b).copied().unwrap_or(false),
                );
                if a_pinned && b_pinned {
                    continue;
                }
                let sign_x = if dx < 0.0 { -1.0 } else { 1.0 };
                let sign_y = if dy < 0.0 { -1.0 } else { 1.0 };
                if overlap_x < overlap_y {
                    let push = overlap_x + EPS;
                    let (dir_a, dir_b) = shifts(a_pinned, b_pinned, sign_x);
                    if dir_a != 0.0 {
                        positions[a].x += dir_a * push;
                    }
                    if dir_b != 0.0 {
                        positions[b].x += dir_b * push;
                    }
                } else {
                    let push = overlap_y + EPS;
                    let (dir_a, dir_b) = shifts(a_pinned, b_pinned, sign_y);
                    if dir_a != 0.0 {
                        positions[a].y += dir_a * push;
                    }
                    if dir_b != 0.0 {
                        positions[b].y += dir_b * push;
                    }
                }
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

/// How far each side of an overlapping pair moves along the axis being split:
/// the pinned side holds, the other takes the whole correction; two free sides
/// share it.
fn shifts(a_pinned: bool, b_pinned: bool, sign: f32) -> (f32, f32) {
    if a_pinned {
        (0.0, sign)
    } else if b_pinned {
        (-sign, 0.0)
    } else {
        (-sign, sign)
    }
}

/// Distance between successive rings. Slightly larger than the force layout's
/// ideal edge length so rings read as clearly separated bands.
pub const RING_GAP: f32 = 12.0;

/// BFS hop-distances from `focus` over an undirected adjacency list. Returns one
/// entry per node: `Some(hops)` if reachable, `None` otherwise.
pub fn bfs_distances(n: usize, focus: usize, adj: &[Vec<usize>]) -> Vec<Option<u32>> {
    let mut dist = vec![None; n];
    if focus >= n {
        return dist;
    }
    dist[focus] = Some(0);
    let mut queue = VecDeque::new();
    queue.push_back(focus);
    while let Some(u) = queue.pop_front() {
        let du = dist[u].unwrap_or(0);
        for &v in &adj[u] {
            if dist[v].is_none() {
                dist[v] = Some(du + 1);
                queue.push_back(v);
            }
        }
    }
    dist
}

/// Compute concentric-ring positions. `dist[i]` is node `i`'s hop-distance from
/// the focus (`None` = unreachable); `adj` is the undirected adjacency used to
/// order each ring by parent angle. The focus (distance 0) lands at the origin.
/// `gap` is the ring spacing per axis, so a terminal's tall cells get rings that
/// read as circles while still packing into a wide pane.
pub fn radial_layout(
    n: usize,
    dist: &[Option<u32>],
    adj: &[Vec<usize>],
    gap_x: f32,
    gap_y: f32,
) -> Vec<Vec2> {
    let mut pos = vec![Vec2::default(); n];
    let mut angle = vec![0.0f32; n];
    if n == 0 {
        return pos;
    }

    let max_finite = dist.iter().flatten().copied().max().unwrap_or(0);

    // Bucket node indices by ring (unreachable nodes share an outer limbo ring).
    let mut rings = std::collections::BTreeMap::<u32, Vec<usize>>::new();
    for (i, d) in dist.iter().enumerate().take(n) {
        let ring = d.unwrap_or(max_finite + 1);
        rings.entry(ring).or_default().push(i);
    }

    // Ascending ring order means a node's parents (ring-1) are placed before it,
    // so their angles are known when we order this ring.
    for (&ring, nodes) in &rings {
        if ring == 0 {
            for &i in nodes {
                pos[i] = Vec2::default();
                angle[i] = 0.0;
            }
            continue;
        }
        let mut ordered = nodes.clone();
        ordered.sort_by(|&a, &b| {
            let pa = parent_angle(a, ring, dist, adj, &angle);
            let pb = parent_angle(b, ring, dist, adj, &angle);
            pa.partial_cmp(&pb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        let m = ordered.len() as f32;
        let rx = ring as f32 * gap_x;
        let ry = ring as f32 * gap_y;
        // Stagger each ring's starting angle so successive rings don't align
        // radially (which would overlap labels and edges).
        let off = ring as f32 * 0.6;
        for (j, &i) in ordered.iter().enumerate() {
            let a = off + std::f32::consts::TAU * (j as f32) / m;
            angle[i] = a;
            pos[i] = Vec2::new(rx * a.cos(), ry * a.sin());
        }
    }
    pos
}

/// Angle of a node's first parent (a neighbour one ring closer to the focus),
/// used to order a ring so children sit near their parent. Falls back to 0.
fn parent_angle(
    i: usize,
    ring: u32,
    dist: &[Option<u32>],
    adj: &[Vec<usize>],
    angle: &[f32],
) -> f32 {
    adj[i]
        .iter()
        .find(|&&p| dist[p] == Some(ring - 1))
        .map(|&p| angle[p])
        .unwrap_or(0.0)
}

/// Where a node with no placed neighbour starts: a golden-angle spiral, so
/// successive arrivals do not stack on the same spot.
pub fn seed_offset(k: usize) -> Vec2 {
    let a = k as f32 * 2.399_963_2;
    let r = 2.0 * (k as f32 + 1.0).sqrt();
    Vec2::new(a.cos() * r, a.sin() * r)
}

/// What detail a node is rendered at, given its distance from the focus and the
/// active detail radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeLod {
    /// The focused node itself.
    Focus,
    /// Inside the radius: full detail.
    Full,
    /// Exactly at the radius: drawn without its label.
    Boundary,
    /// Beyond the radius (or unreachable): not drawn.
    Hidden,
}

/// Map the geometric zoom factor to a semantic detail radius.
/// `None` means "overview" — show the whole graph (no hop filtering).
pub fn detail_radius(zoom: f32) -> Option<u32> {
    if zoom < 1.5 {
        None
    } else if zoom < 2.3 {
        Some(3)
    } else if zoom < 3.5 {
        Some(2)
    } else {
        Some(1)
    }
}

/// Human-readable name for a tier, shown in the canvas title.
pub fn tier_label(radius: Option<u32>) -> String {
    match radius {
        None => "overview".to_string(),
        Some(1) => "focus+ring".to_string(),
        Some(n) => format!("{n} hops"),
    }
}

/// Classify a node by its `distance` from the focus (`None` = unreachable)
/// under the active `radius` (`None` = overview, everything visible).
pub fn classify(distance: Option<u32>, radius: Option<u32>) -> NodeLod {
    match radius {
        None => match distance {
            Some(0) => NodeLod::Focus,
            _ => NodeLod::Full,
        },
        Some(r) => match distance {
            None => NodeLod::Hidden,
            Some(0) => NodeLod::Focus,
            Some(d) if d < r => NodeLod::Full,
            Some(d) if d == r => NodeLod::Boundary,
            Some(_) => NodeLod::Hidden,
        },
    }
}

/// Distance between the two points, for the tests and the callers that need it.
pub fn span(a: Vec2, b: Vec2) -> f32 {
    a.distance(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> LayoutParams {
        LayoutParams {
            ideal_len: 40.0,
            repulsion: 1.0,
        }
    }

    #[test]
    fn repulsion_pushes_coincident_nodes_apart() {
        let mut pos = vec![Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0)];
        let pinned = [false, false];
        step(&mut pos, &[], &pinned, 5.0, &params());
        assert!(span(pos[0], pos[1]) > 0.0, "{pos:?}");
    }

    #[test]
    fn an_edge_settles_near_its_ideal_length() {
        let mut pos = vec![Vec2::new(0.0, 0.0), Vec2::new(3.0, 0.0)];
        let pinned = [true, false];
        let edges = [(0, 1)];
        settle(&mut pos, &edges, &pinned, &params());
        let distance = span(pos[0], pos[1]);
        assert!(
            (distance - 40.0).abs() < 8.0,
            "an edge rests near k, got {distance}"
        );
        assert_eq!(pos[0], Vec2::new(0.0, 0.0), "the pinned node holds still");
    }

    #[test]
    fn a_pinned_node_never_moves() {
        let mut pos = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
        ];
        let pinned = [true, false, false];
        let before = pos[0];
        step(&mut pos, &[(0, 1), (0, 2)], &pinned, 50.0, &params());
        assert_eq!(pos[0], before);
    }

    #[test]
    fn the_radial_seed_puts_the_focus_at_the_origin_and_rings_on_their_radius() {
        // a is the focus; b and c hang off it; d hangs off b.
        let adj = vec![vec![1, 2], vec![0, 3], vec![0], vec![1]];
        let dist = bfs_distances(4, 0, &adj);
        assert_eq!(dist, vec![Some(0), Some(1), Some(1), Some(2)]);
        let pos = radial_layout(4, &dist, &adj, RING_GAP, RING_GAP);
        assert_eq!(pos[0], Vec2::new(0.0, 0.0));
        assert!((span(pos[0], pos[1]) - RING_GAP).abs() < 0.01, "{pos:?}");
        assert!((span(pos[0], pos[2]) - RING_GAP).abs() < 0.01, "{pos:?}");
        assert!(
            (span(pos[0], pos[3]) - 2.0 * RING_GAP).abs() < 0.01,
            "{pos:?}"
        );
    }

    #[test]
    fn unreachable_nodes_take_a_ring_beyond_the_furthest_one() {
        // d is not connected to the focus, so it lands one ring past c.
        let adj = vec![vec![1], vec![0], vec![3], vec![2]];
        let dist = bfs_distances(4, 0, &adj);
        assert_eq!(dist[2], None);
        let pos = radial_layout(4, &dist, &adj, RING_GAP, RING_GAP);
        assert!(
            (span(pos[0], pos[2]) - 2.0 * RING_GAP).abs() < 0.01,
            "{pos:?}"
        );
    }

    #[test]
    fn settling_the_same_seed_twice_gives_the_same_map() {
        let adj = vec![vec![1, 2], vec![0, 3], vec![0], vec![1, 2]];
        let dist = bfs_distances(4, 0, &adj);
        let edges = [(0, 1), (0, 2), (1, 3), (2, 3)];
        let pinned = [true, false, false, false];
        let mut first = radial_layout(4, &dist, &adj, RING_GAP, RING_GAP);
        let mut second = radial_layout(4, &dist, &adj, RING_GAP, RING_GAP);
        settle(&mut first, &edges, &pinned, &params());
        settle(&mut second, &edges, &pinned, &params());
        assert_eq!(first, second);
    }

    #[test]
    fn separation_clears_an_overlap_and_leaves_a_pinned_node_alone() {
        let mut pos = vec![Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0)];
        let pinned = [true, false];
        separate(&mut pos, 10.0, 4.0, &pinned, 8);
        assert_eq!(pos[0], Vec2::new(0.0, 0.0));
        assert!(
            (pos[1].x - pos[0].x).abs() >= 20.0 || (pos[1].y - pos[0].y).abs() >= 8.0,
            "the boxes no longer overlap: {pos:?}"
        );
        // Already clear: a second pass is a no-op.
        let before = pos.clone();
        separate(&mut pos, 10.0, 4.0, &pinned, 8);
        assert_eq!(pos, before);
    }

    #[test]
    fn the_zoom_tiers_pick_the_detail_radius() {
        assert_eq!(detail_radius(1.0), None);
        assert_eq!(detail_radius(1.4), None);
        assert_eq!(detail_radius(1.5), Some(3));
        assert_eq!(detail_radius(2.3), Some(2));
        assert_eq!(detail_radius(3.5), Some(1));
        assert_eq!(tier_label(None), "overview");
        assert_eq!(tier_label(Some(2)), "2 hops");
        assert_eq!(tier_label(Some(1)), "focus+ring");
    }

    #[test]
    fn the_tier_hides_what_lies_past_the_radius() {
        assert_eq!(classify(Some(0), None), NodeLod::Focus);
        assert_eq!(classify(Some(4), None), NodeLod::Full);
        assert_eq!(classify(None, None), NodeLod::Full);
        assert_eq!(classify(Some(0), Some(2)), NodeLod::Focus);
        assert_eq!(classify(Some(1), Some(2)), NodeLod::Full);
        assert_eq!(classify(Some(2), Some(2)), NodeLod::Boundary);
        assert_eq!(classify(Some(3), Some(2)), NodeLod::Hidden);
        assert_eq!(classify(None, Some(2)), NodeLod::Hidden);
    }

    #[test]
    fn the_seed_spiral_hands_every_newcomer_its_own_spot() {
        let offsets: Vec<Vec2> = (0..6).map(seed_offset).collect();
        for (i, a) in offsets.iter().enumerate() {
            for b in &offsets[i + 1..] {
                assert!(span(*a, *b) > 0.5, "{offsets:?}");
            }
        }
    }
}
