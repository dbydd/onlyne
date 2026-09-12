// Derived in part from graphtatui (c) Sok205 contributors, MIT/Apache-2.0.
//! The page-1 role map: an egocentric force layout (the ported geometry lives
//! in [`crate::force`]) projected into terminal cells, drawn as labelled boxes
//! joined by orthogonal, arrow-tipped hops.
//!
//! The map is a *value of the topology*: [`RoleMap::sync`] re-seeds and
//! relaxes only when the sorted role set, the edge set, or the repulsion knob
//! changes, and it seeds newcomers beside the roles they talk to, so a refresh
//! never shuffles the picture under the operator's cursor.

pub use crate::force::tier_label;
use crate::force::{self, LayoutParams, NodeLod, Vec2};
use crate::model::clamp_spacing;
use std::collections::{BTreeMap, BTreeSet};

/// The widest a role box draws. A box's own text only asks for what it needs,
/// so this is the ceiling the width starts from.
const NODE_W: usize = 28;
/// The narrowest box that still shows a title and a session line.
const NODE_MIN_W: usize = 14;
/// Box height: a title row, up to two session rows, and the borders.
const NODE_H: usize = 5;
/// Clear space the force layout keeps between two neighbouring boxes, on top of
/// the box itself. A terminal pane is far smaller than the graph, so the pitch
/// stays near the box size and the camera pans over the rest.
const IDEAL_GAP: f32 = 3.0;
/// Ring spacing on top of the box height, so hop bands read as bands.
const RING_GAP_EXTRA: f32 = 2.0;
/// What one layout unit of the vertical axis covers in cells. A terminal cell is
/// about twice as tall as it is wide, so a map circle stays readable only if
/// eight tenths of a unit fits one row: rings come out round on screen and the
/// map stays inside a wide, short pane.
pub const Y_CELL_SCALE: f32 = 0.8;
/// Sweeps the anti-overlap pass may spend bringing boxes apart.
const SEPARATION_SWEEPS: usize = 12;
/// Manual zoom controls. One layout unit is one cell at zoom 1; `0` resets to
/// this and centres the focused role.
pub const ZOOM_STEP: f32 = 1.2;
pub const MIN_ZOOM: f32 = 0.2;
pub const MAX_ZOOM: f32 = 8.0;
/// How far either side of the midpoint a hop looks for a free lane.
const LANE_SCAN: isize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutNode {
    pub name: String,
    pub title: String,
    pub presence: Presence,
    pub sessions: Vec<SessionLine>,
    pub aggregate: Option<String>,
    pub busy: bool,
}

impl LayoutNode {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            title: name.clone(),
            name,
            presence: Presence::Offline,
            sessions: Vec::new(),
            aggregate: None,
            busy: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLine {
    pub task: String,
    pub state: SessionState,
    pub age: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Created,
    Working,
    Idle,
    Exited,
}

impl SessionState {
    pub fn glyph(self) -> char {
        match self {
            SessionState::Created => '◌',
            SessionState::Working => '◐',
            SessionState::Idle => '◔',
            SessionState::Exited => '●',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Online,
    Offline,
    Draining,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutEdge {
    pub from: String,
    pub to: String,
    pub in_flight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Plain,
    Online,
    Offline,
    Draining,
    Busy,
    Edge,
    ActiveEdge,
    Aggregate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub kind: CellKind,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            kind: CellKind::Plain,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Vec<Cell>>,
    pub node_boxes: Vec<NodeBox>,
    /// The cells each drawn hop took, keyed by `(from, to)`. The pane reverses
    /// the hop `j`/`k` stands on from these coordinates.
    pub edge_paths: BTreeMap<(String, String), Vec<(isize, isize)>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeBox {
    pub name: String,
    /// The box's corner on the pane, in cells. Negative coordinates are
    /// ordinary: the pane is a window over a larger map.
    pub x: isize,
    pub y: isize,
    pub w: isize,
    pub h: isize,
    /// The text drawn on the box's title row and the column it starts at. The
    /// placement never fills them; the drawing pass does.
    pub label: String,
    pub label_x: isize,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![vec![Cell::blank(); width]; height],
            node_boxes: Vec::new(),
            edge_paths: BTreeMap::new(),
        }
    }

    /// The cell at a world coordinate. Anything outside the drawn area reads
    /// as blank.
    pub fn at(&self, x: isize, y: isize) -> Cell {
        if x < 0 || y < 0 {
            return Cell::blank();
        }
        self.cells
            .get(y as usize)
            .and_then(|row| row.get(x as usize))
            .copied()
            .unwrap_or_else(Cell::blank)
    }

    fn holds(&self, cell: (isize, isize)) -> bool {
        cell.0 >= 0
            && cell.1 >= 0
            && (cell.0 as usize) < self.width
            && (cell.1 as usize) < self.height
    }

    pub fn lines(&self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines()
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The box size every role draws at: the longest line a role shows, held
/// between the width that still reads and the width the map has room for.
pub fn box_width(nodes: &[LayoutNode]) -> usize {
    let longest = nodes.iter().map(longest_line).max().unwrap_or(8);
    (longest + 4).clamp(NODE_MIN_W, NODE_W)
}

fn longest_line(node: &LayoutNode) -> usize {
    let mut longest =
        node.title.chars().count() + usize::from(node.aggregate.is_some()) + usize::from(node.busy);
    for session in node.sessions.iter().take(2) {
        longest =
            longest.max(session.task.chars().count().min(8) + session.age.chars().count() + 4);
    }
    if node.sessions.len() > 2 {
        longest = longest.max(3);
    }
    longest
}

/// Repulsion the `+`/`-` knob asks for: level 2 (the default) is the upstream
/// default of 1.0.
pub fn repulsion_for(spacing: usize) -> f32 {
    0.5 * clamp_spacing(spacing) as f32
}

/// The page-1 layout: where every role sits, what the focus is, and how far
/// each role lies from it. Rebuilt only when the topology or the repulsion
/// changes.
#[derive(Clone, Debug)]
pub struct RoleMap {
    positions: BTreeMap<String, Vec2>,
    names: Vec<String>,
    edges: Vec<(usize, usize)>,
    hash: u64,
    synced: bool,
    focus: Option<String>,
    distances: BTreeMap<String, u32>,
    params: LayoutParams,
    ring_gap_x: f32,
    ring_gap_y: f32,
    repulsion_level: usize,
}

impl Default for RoleMap {
    fn default() -> Self {
        Self {
            positions: BTreeMap::new(),
            names: Vec::new(),
            edges: Vec::new(),
            hash: 0,
            synced: false,
            focus: None,
            distances: BTreeMap::new(),
            params: LayoutParams::default(),
            ring_gap_x: force::RING_GAP,
            ring_gap_y: force::RING_GAP,
            repulsion_level: 0,
        }
    }
}

impl RoleMap {
    /// Bring the map in line with the roles and hops on screen. Returns
    /// whether anything moved: an unchanged topology and knob leave the map
    /// exactly as it was.
    pub fn sync(&mut self, nodes: &[LayoutNode], edges: &[LayoutEdge], spacing: usize) -> bool {
        let names: Vec<String> = nodes
            .iter()
            .map(|node| node.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let index: BTreeMap<String, usize> = names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i))
            .collect();
        let canonical: Vec<(usize, usize)> = edges
            .iter()
            .filter_map(|edge| {
                let from = *index.get(&edge.from)?;
                let to = *index.get(&edge.to)?;
                (from != to).then_some((from.min(to), from.max(to)))
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let hash = topology_hash(&names, &canonical);
        let repulsion = repulsion_for(spacing);
        if self.synced && hash == self.hash && spacing == self.repulsion_level {
            return false;
        }
        self.names = names;
        self.edges = canonical;
        self.hash = hash;
        self.synced = true;
        self.repulsion_level = spacing;

        let node_w = box_width(nodes) as f32;
        let node_h = NODE_H as f32;
        self.params = LayoutParams {
            ideal_len: node_w + IDEAL_GAP,
            repulsion,
        };
        self.ring_gap_x = node_w + IDEAL_GAP;
        self.ring_gap_y = (node_h + RING_GAP_EXTRA) / Y_CELL_SCALE;

        self.positions.retain(|name, _| index.contains_key(name));

        let mut adj = vec![Vec::new(); self.names.len()];
        for &(a, b) in &self.edges {
            adj[a].push(b);
            adj[b].push(a);
        }
        self.focus = self
            .focus
            .take()
            .filter(|name| index.contains_key(name.as_str()))
            .or_else(|| hub(&self.names, &adj));
        let focus_idx = self
            .focus
            .as_ref()
            .and_then(|name| index.get(name).copied());
        let dist = focus_idx
            .map(|focus| force::bfs_distances(self.names.len(), focus, &adj))
            .unwrap_or_else(|| vec![None; self.names.len()]);
        self.distances = self
            .names
            .iter()
            .enumerate()
            .filter_map(|(i, name)| dist[i].map(|hops| (name.clone(), hops)))
            .collect();

        // A fresh map falls out of the hop-distance rings; an existing one
        // keeps every role where the operator last saw it and drops the
        // newcomers beside the roles they talk to.
        if self.positions.is_empty() {
            let seeded = force::radial_layout(
                self.names.len(),
                &dist,
                &adj,
                self.ring_gap_x,
                self.ring_gap_y,
            );
            for (i, name) in self.names.iter().enumerate() {
                self.positions.insert(name.clone(), seeded[i]);
            }
        } else {
            let mut arrivals = Vec::new();
            for (i, name) in self.names.iter().enumerate() {
                if self.positions.contains_key(name) {
                    continue;
                }
                let base = adj[i]
                    .iter()
                    .filter_map(|j| self.positions.get(&self.names[*j]).copied())
                    .fold(None::<(Vec2, usize)>, |acc, point| match acc {
                        None => Some((point, 1)),
                        Some((sum, count)) => {
                            Some((Vec2::new(sum.x + point.x, sum.y + point.y), count + 1))
                        }
                    })
                    .map(|(sum, count)| Vec2::new(sum.x / count as f32, sum.y / count as f32))
                    .or_else(|| {
                        dist[i].map(|hops| {
                            let angle = (i as f32) * 1.7;
                            let rx = hops as f32 * self.ring_gap_x;
                            let ry = hops as f32 * self.ring_gap_y;
                            Vec2::new(rx * angle.cos(), ry * angle.sin())
                        })
                    })
                    .unwrap_or_default();
                let seed = force::seed_offset(i);
                arrivals.push((name.clone(), Vec2::new(base.x + seed.x, base.y + seed.y)));
            }
            for (name, point) in arrivals {
                self.positions.insert(name, point);
            }
        }

        let mut points: Vec<Vec2> = self.names.iter().map(|name| self.positions[name]).collect();
        let pinned: Vec<bool> = self
            .names
            .iter()
            .map(|name| self.focus.as_deref() == Some(name.as_str()))
            .collect();
        // Settle before the first frame draws, so the map never animates from
        // its seed, then size it to its boxes and shove those boxes apart.
        force::settle(&mut points, &self.edges, &pinned, &self.params);
        fit(&mut points, node_w, node_h, repulsion);
        force::separate(
            &mut points,
            node_w / 2.0 + 1.0,
            (node_h / 2.0 + 1.0) / Y_CELL_SCALE,
            &pinned,
            SEPARATION_SWEEPS,
        );
        for (name, point) in self.names.iter().zip(points) {
            self.positions.insert(name.clone(), point);
        }
        true
    }

    pub fn positions(&self) -> &BTreeMap<String, Vec2> {
        &self.positions
    }

    pub fn position(&self, name: &str) -> Option<Vec2> {
        self.positions.get(name).copied()
    }

    /// The role the layout is centred on: the busiest hub of the topology, or
    /// the role the last topology kept.
    pub fn focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// How many hops `name` lies from the focus; `None` when unreachable or
    /// unknown.
    pub fn distance(&self, name: &str) -> Option<u32> {
        self.distances.get(name).copied()
    }

    /// The semantic detail radius the zoom tier asks for.
    pub fn radius(&self, zoom: f32) -> Option<u32> {
        force::detail_radius(zoom)
    }

    pub fn repulsion(&self) -> f32 {
        self.params.repulsion
    }

    /// The point the camera centres on: the named role, else the focus, else
    /// the origin of the rings.
    pub fn anchor(&self, name: Option<&str>) -> Vec2 {
        name.and_then(|name| self.position(name))
            .or_else(|| self.focus().and_then(|name| self.position(name)))
            .unwrap_or_default()
    }

    /// The map's extent for these role boxes, so the camera can stop at its
    /// edge.
    pub fn extent(&self, nodes: &[LayoutNode]) -> Option<(Vec2, Vec2)> {
        self.bounds(box_width(nodes), NODE_H)
    }

    /// The middle of the map's extent. The camera frames this when nothing has
    /// been panned, so the whole neighbourhood is on screen instead of one
    /// role's corner of it.
    pub fn centre(&self, nodes: &[LayoutNode]) -> Vec2 {
        match self.extent(nodes) {
            Some((min, max)) => Vec2::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0),
            None => Vec2::default(),
        }
    }

    /// The extent every box covers, so the camera can stop at the map's edge.
    pub fn bounds(&self, node_w: usize, node_h: usize) -> Option<(Vec2, Vec2)> {
        let half_w = node_w as f32 / 2.0;
        let half_h = node_h as f32 / 2.0;
        self.positions.values().fold(None, |acc, point| {
            let low = Vec2::new(point.x - half_w, point.y - half_h);
            let high = Vec2::new(point.x + half_w, point.y + half_h);
            Some(match acc {
                None => (low, high),
                Some((min, max)) => (
                    Vec2::new(min.x.min(low.x), min.y.min(low.y)),
                    Vec2::new(max.x.max(high.x), max.y.max(high.y)),
                ),
            })
        })
    }
}

/// Normalise the settled map so its size comes from the boxes rather than from
/// the force layout's arbitrary units: `n` boxes want about `sqrt(n)` of them
/// per side, which is the shape that frames a terminal pane. The shape itself
/// is untouched — this only scales it, and the repulsion knob opens the result
/// out, so a wider knob shows as a wider map.
fn fit(points: &mut [Vec2], node_w: f32, node_h: f32, repulsion: f32) {
    let n = points.len() as f32;
    if points.is_empty() {
        return;
    }
    let mut min = points[0];
    let mut max = points[0];
    for point in points.iter() {
        min = Vec2::new(min.x.min(point.x), min.y.min(point.y));
        max = Vec2::new(max.x.max(point.x), max.y.max(point.y));
    }
    let span_x = max.x - min.x;
    let span_y = (max.y - min.y) * Y_CELL_SCALE;
    let spread = 0.7 + 0.3 * repulsion.max(0.25);
    let scale_x = if span_x > 1e-3 {
        node_w * 1.6 * n.sqrt() * spread / span_x
    } else {
        1.0
    };
    let scale_y = if span_y > 1e-3 {
        node_h * 1.5 * n.sqrt() * spread / span_y
    } else {
        1.0
    };
    let centre = Vec2::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
    for point in points.iter_mut() {
        point.x = centre.x + (point.x - centre.x) * scale_x;
        point.y = centre.y + (point.y - centre.y) * scale_y;
    }
}

/// The role with the most hops to it; ties go to the first name, so the pick
/// is a pure function of the topology.
fn hub(names: &[String], adj: &[Vec<usize>]) -> Option<String> {
    names
        .iter()
        .enumerate()
        .max_by_key(|(i, _)| (adj[*i].len(), std::cmp::Reverse(*i)))
        .map(|(_, name)| name.clone())
}

/// FNV-1a over the sorted role names and hop set: the map rebuilds when, and
/// only when, this changes.
fn topology_hash(names: &[String], edges: &[(usize, usize)]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |byte: u8| {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for name in names {
        for byte in name.as_bytes() {
            eat(*byte);
        }
        eat(0);
    }
    for (from, to) in edges {
        for byte in from.to_le_bytes().into_iter().chain(to.to_le_bytes()) {
            eat(byte);
        }
    }
    hash
}

/// The camera: a zoom factor over the layout's units and a pan offset in
/// cells. `zoom == 1.0` draws one layout unit per cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub zoom: f32,
    pub pan: (isize, isize),
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (0, 0),
        }
    }
}

impl Camera {
    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * ZOOM_STEP).min(MAX_ZOOM);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / ZOOM_STEP).max(MIN_ZOOM);
    }

    /// Back to 1:1, centred on the focused role.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Where a point of the map lands on the pane: the anchor sits at the
    /// pane's middle, and the pan slides it. The vertical axis is divided by
    /// [`Y_CELL_SCALE`], so a ring of the layout reads as a circle on cells that
    /// are twice as tall as they are wide.
    pub fn project(&self, world: Vec2, anchor: Vec2, view: (usize, usize)) -> (isize, isize) {
        let cx = view.0 as f32 / 2.0;
        let cy = view.1 as f32 / 2.0;
        (
            ((world.x - anchor.x) * self.zoom + cx).round() as isize + self.pan.0,
            ((world.y - anchor.y) * self.zoom * Y_CELL_SCALE + cy).round() as isize + self.pan.1,
        )
    }

    /// Keep the pane over the map: the view window never leaves the content,
    /// so panning can always bring a role back into view but never loses them
    /// all.
    pub fn clamp_pan(&mut self, bounds: (Vec2, Vec2), anchor: Vec2, view: (usize, usize)) {
        let half_x = view.0 as f32 / 2.0;
        let half_y = view.1 as f32 / 2.0;
        let scale_x = self.zoom;
        let scale_y = self.zoom * Y_CELL_SCALE;
        self.pan.0 = clamp_axis(
            self.pan.0,
            (bounds.0.x - anchor.x) * scale_x - half_x,
            (bounds.1.x - anchor.x) * scale_x + half_x,
        );
        self.pan.1 = clamp_axis(
            self.pan.1,
            (bounds.0.y - anchor.y) * scale_y - half_y,
            (bounds.1.y - anchor.y) * scale_y + half_y,
        );
    }
}

fn clamp_axis(value: isize, a: f32, b: f32) -> isize {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    (value as f32).min(high).max(low).round() as isize
}

/// Draw the map: every role the zoom tier shows, its box placed by the
/// projection, and every hop routed orthogonally into its target.
pub fn canvas(
    map: &RoleMap,
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    camera: &Camera,
    anchor: Vec2,
    view: (usize, usize),
) -> Canvas {
    let (width, height) = view;
    let mut canvas = Canvas::new(width, height);
    if width < 12 || height < 4 || nodes.is_empty() {
        draw_text(&mut canvas, 0, 0, "(no roles)", CellKind::Plain);
        return canvas;
    }
    let node_w = box_width(nodes).min(width);
    let node_h = NODE_H.min(height);
    let radius = map.radius(camera.zoom);

    let mut ordered: Vec<&LayoutNode> = nodes.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    let mut placed: Vec<(&LayoutNode, Rect)> = Vec::new();
    for node in ordered {
        let Some(world) = map.position(&node.name) else {
            continue;
        };
        if matches!(
            force::classify(map.distance(&node.name), radius),
            NodeLod::Hidden
        ) {
            continue;
        }
        let (cx, cy) = camera.project(world, anchor, view);
        let rect = Rect {
            x: cx - node_w as isize / 2,
            y: cy - node_h as isize / 2,
            w: node_w as isize,
            h: node_h as isize,
        };
        // A box whose label the pane can show still draws, even at the edge:
        // the pane is a window onto the map, not a frame it has to fit.
        if rect.labels(width, height) {
            placed.push((node, rect));
        }
    }
    let boxes: BTreeMap<&str, Rect> = placed
        .iter()
        .map(|(node, rect)| (node.name.as_str(), *rect))
        .collect();
    let obstacles: Vec<Rect> = placed.iter().map(|(_, rect)| *rect).collect();

    // Hops first, so a box border stays on top of every line that reaches it;
    // their arrowheads last, so two hops sharing a lane cannot erase the
    // other's direction.
    let mut tips = Vec::new();
    for edge in edges {
        let (Some(from), Some(to)) = (boxes.get(edge.from.as_str()), boxes.get(edge.to.as_str()))
        else {
            continue;
        };
        let kind = if edge.in_flight {
            CellKind::ActiveEdge
        } else {
            CellKind::Edge
        };
        let route = route(*from, *to, &obstacles);
        if route.is_empty() {
            continue;
        }
        let mut path = Vec::new();
        for (index, cell) in route.iter().enumerate() {
            if inside_any(&obstacles, *cell) {
                continue;
            }
            if index + 1 == route.len() {
                tips.push((*cell, arrowhead(*to, *cell), kind));
            } else if canvas.holds(*cell) {
                put_at(&mut canvas, *cell, turn(&route, index), kind);
            }
            path.push(*cell);
        }
        canvas
            .edge_paths
            .insert((edge.from.clone(), edge.to.clone()), path);
    }
    for (node, rect) in &placed {
        let (label, label_x) = draw_node(&mut canvas, node, *rect);
        canvas.node_boxes.push(NodeBox {
            name: node.name.clone(),
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            label,
            label_x,
        });
    }
    for (cell, ch, kind) in tips {
        if canvas.holds(cell) {
            put_at(&mut canvas, cell, ch, kind);
        }
    }
    erase_islands(&mut canvas, &obstacles);
    canvas
}

/// A box's rectangle in cell coordinates; negative coordinates are ordinary
/// while the camera is off the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: isize,
    y: isize,
    w: isize,
    h: isize,
}

impl Rect {
    fn cx(&self) -> isize {
        self.x + self.w / 2
    }

    fn cy(&self) -> isize {
        self.y + self.h / 2
    }

    fn contains(&self, cell: (isize, isize)) -> bool {
        cell.0 >= self.x && cell.0 < self.x + self.w && cell.1 >= self.y && cell.1 < self.y + self.h
    }

    /// Whether the box belongs on a pane of this size: its title row is on
    /// screen and the label starts inside the pane. A box that would show only
    /// a sliver of border is left off instead, so a role never reads as a
    /// stray line.
    fn labels(&self, width: usize, height: usize) -> bool {
        self.y >= 0
            && self.y < height as isize
            && self.x + 2 >= 0
            && self.x + 4 < width as isize
            && self.x + self.w > 0
    }
}

/// Draw one role's box and report the title it wrote and where it starts, so
/// the pane can reverse the cursor's label on top. Cells outside the pane are
/// skipped, so a box at the edge draws the part that shows.
fn draw_node(canvas: &mut Canvas, node: &LayoutNode, rect: Rect) -> (String, isize) {
    let border = match node.presence {
        Presence::Online => CellKind::Online,
        Presence::Offline => CellKind::Offline,
        Presence::Draining => CellKind::Draining,
    };
    if rect.w < 2 || rect.h < 2 {
        return (String::new(), rect.x);
    }
    for y in rect.y + 1..rect.y + rect.h - 1 {
        for x in rect.x + 1..rect.x + rect.w - 1 {
            put_at(canvas, (x, y), ' ', CellKind::Plain);
        }
    }
    put_at(canvas, (rect.x, rect.y), '╭', border);
    for x in rect.x + 1..rect.x + rect.w - 1 {
        put_at(canvas, (x, rect.y), '─', border);
    }
    put_at(canvas, (rect.x + rect.w - 1, rect.y), '╮', border);
    for y in rect.y + 1..rect.y + rect.h - 1 {
        put_at(canvas, (rect.x, y), '│', border);
        put_at(canvas, (rect.x + rect.w - 1, y), '│', border);
    }
    put_at(canvas, (rect.x, rect.y + rect.h - 1), '╰', border);
    for x in rect.x + 1..rect.x + rect.w - 1 {
        put_at(canvas, (x, rect.y + rect.h - 1), '─', border);
    }
    put_at(
        canvas,
        (rect.x + rect.w - 1, rect.y + rect.h - 1),
        '╯',
        border,
    );

    let text_width = (rect.w - 4).max(0) as usize;
    let mut title = String::new();
    if node.aggregate.is_some() {
        title.push('⬡');
    }
    title.push_str(&node.title);
    let title = truncate(&title, text_width.saturating_sub(usize::from(node.busy)));
    let label_x = rect.x + 2;
    draw_text(
        canvas,
        rect.y,
        label_x,
        &title,
        if node.aggregate.is_some() {
            CellKind::Aggregate
        } else {
            CellKind::Plain
        },
    );
    if node.busy {
        let star_x = label_x + char_len(&title) as isize;
        if star_x < rect.x + rect.w - 1 {
            put_at(canvas, (star_x, rect.y), '*', CellKind::Busy);
        }
    }

    let visible = node.sessions.iter().take(2).collect::<Vec<_>>();
    for (idx, session) in visible.iter().enumerate() {
        let text = format!(
            "{} {} {}",
            short(&session.task, 8),
            session.state.glyph(),
            session.age
        );
        draw_text(
            canvas,
            rect.y + 1 + idx as isize,
            label_x,
            &truncate(&text, text_width),
            CellKind::Plain,
        );
    }
    if node.sessions.len() > visible.len() && rect.h > 4 {
        let text = format!("+{}", node.sessions.len() - visible.len());
        draw_text(canvas, rect.y + 3, label_x, &text, CellKind::Plain);
    }
    (title.into_owned(), label_x)
}

fn draw_text(canvas: &mut Canvas, y: isize, x: isize, text: &str, kind: CellKind) {
    for (idx, ch) in text.chars().enumerate() {
        put_at(canvas, (x + idx as isize, y), ch, kind);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Up,
    Down,
}

/// The cell just outside one border: where a hop leaves a box and where it
/// lands against its target.
fn port(rect: Rect, side: Side) -> (isize, isize) {
    match side {
        Side::Left => (rect.x - 1, rect.cy()),
        Side::Right => (rect.x + rect.w, rect.cy()),
        Side::Up => (rect.cx(), rect.y - 1),
        Side::Down => (rect.cx(), rect.y + rect.h),
    }
}

/// The arrowhead a hop lands with, from the border it enters: the glyph points
/// the way the last run travels.
fn arrowhead(target: Rect, cell: (isize, isize)) -> char {
    if cell.0 + 1 == target.x {
        '▸'
    } else if cell.0 == target.x + target.w {
        '◂'
    } else if cell.1 + 1 == target.y {
        '▾'
    } else if cell.1 == target.y + target.h {
        '▴'
    } else {
        '▸'
    }
}

/// One candidate hop: how many box cells it crosses, how long it runs, and the
/// cells themselves. The cheapest wins, so a hop crosses boxes only when it
/// cannot avoid them.
type Candidate = (usize, usize, Vec<(isize, isize)>);

/// Route one hop: horizontal and vertical runs only, at most two corners, with
/// the crossing on the midpoint between the two ports.
fn route(from: Rect, to: Rect, obstacles: &[Rect]) -> Vec<(isize, isize)> {
    let dx = to.cx() - from.cx();
    let dy = to.cy() - from.cy();
    let h_exit = if dx >= 0 { Side::Right } else { Side::Left };
    let h_enter = if from.cx() <= to.cx() {
        Side::Left
    } else {
        Side::Right
    };
    let v_exit = if dy >= 0 { Side::Down } else { Side::Up };
    let v_enter = if from.cy() <= to.cy() {
        Side::Up
    } else {
        Side::Down
    };
    let pairs = if dx.abs() >= dy.abs() {
        [
            (h_exit, h_enter),
            (v_exit, v_enter),
            (h_exit, v_enter),
            (v_exit, h_enter),
        ]
    } else {
        [
            (v_exit, v_enter),
            (h_exit, h_enter),
            (v_exit, h_enter),
            (h_exit, v_enter),
        ]
    };
    let mut best: Option<Candidate> = None;
    for (exit, enter) in pairs {
        for points in waypoints(exit, enter, from, to) {
            let cells = expand(&points);
            let hits = cells
                .iter()
                .filter(|cell| inside_any(obstacles, **cell))
                .count();
            let candidate = (hits, cells.len(), cells);
            let better = match &best {
                None => true,
                Some(best) => (candidate.0, candidate.1) < (best.0, best.1),
            };
            if better {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, cells)| cells).unwrap_or_default()
}

/// The corner points of one candidate route. Exits and entries come in pairs:
/// two horizontal ends cross on a column, two vertical ends on a row, and a
/// mixed pair turns once.
fn waypoints(exit: Side, enter: Side, from: Rect, to: Rect) -> Vec<Vec<(isize, isize)>> {
    let start = port(from, exit);
    let tip = port(to, enter);
    match (exit, enter) {
        (Side::Left | Side::Right, Side::Left | Side::Right) => midlines(start.0, tip.0)
            .into_iter()
            .map(|mid| vec![start, (mid, start.1), (mid, tip.1), tip])
            .collect(),
        (Side::Up | Side::Down, Side::Up | Side::Down) => midlines(start.1, tip.1)
            .into_iter()
            .map(|mid| vec![start, (start.0, mid), (tip.0, mid), tip])
            .collect(),
        (Side::Left | Side::Right, _) => vec![vec![start, (tip.0, start.1), tip]],
        (_, _) => vec![vec![start, (start.0, tip.1), tip]],
    }
}

/// The candidate crossing points: the midpoint first, then its neighbours
/// outward, so a hop looks for the nearest free lane without losing the
/// two-corner shape.
fn midlines(a: isize, b: isize) -> Vec<isize> {
    let mid = (a + b) / 2;
    let mut out = Vec::with_capacity((2 * LANE_SCAN + 1) as usize);
    for step in 0..=LANE_SCAN {
        if step == 0 {
            out.push(mid);
        } else {
            out.push(mid - step);
            out.push(mid + step);
        }
    }
    out
}

/// Walk the corner points in order, so the cells come back in the order the
/// hop travels: source first, target last.
fn expand(points: &[(isize, isize)]) -> Vec<(isize, isize)> {
    let mut corners: Vec<(isize, isize)> = Vec::new();
    for point in points {
        if corners.last() != Some(point) {
            corners.push(*point);
        }
    }
    let mut cells: Vec<(isize, isize)> = Vec::new();
    for pair in corners.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.1 == b.1 {
            let step = if b.0 >= a.0 { 1isize } else { -1 };
            let mut x = a.0;
            while x != b.0 + step {
                push_cell(&mut cells, (x, a.1));
                x += step;
            }
        } else {
            let step = if b.1 >= a.1 { 1isize } else { -1 };
            let mut y = a.1;
            while y != b.1 + step {
                push_cell(&mut cells, (a.0, y));
                y += step;
            }
        }
    }
    if cells.is_empty() && !corners.is_empty() {
        cells.push(corners[0]);
    }
    cells
}

fn push_cell(cells: &mut Vec<(isize, isize)>, cell: (isize, isize)) {
    if cells.last() != Some(&cell) {
        cells.push(cell);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

fn direction(from: (isize, isize), to: (isize, isize)) -> Dir {
    if to.1 < from.1 {
        Dir::Up
    } else if to.1 > from.1 {
        Dir::Down
    } else if to.0 < from.0 {
        Dir::Left
    } else {
        Dir::Right
    }
}

/// The corner glyph where two runs meet, drawn in the same rounded style as
/// the boxes. An entry direction and an exit direction are not interchangeable:
/// entering left then leaving south (`(Left,Down)`) is the mirror of entering
/// south then leaving east (`(Down,Right)`), so each of the eight turns names
/// its own corner.
fn join(into: Dir, out: Dir) -> char {
    match (into, out) {
        // Leave east and south: top-left corner.
        (Dir::Left, Dir::Down) | (Dir::Up, Dir::Right) => '╭',
        // Leave west and south: top-right corner.
        (Dir::Right, Dir::Down) | (Dir::Up, Dir::Left) => '╮',
        // Leave west and north: bottom-right corner.
        (Dir::Down, Dir::Left) | (Dir::Right, Dir::Up) => '╯',
        // Leave east and north: bottom-left corner.
        (Dir::Down, Dir::Right) | (Dir::Left, Dir::Up) => '╰',
        _ => {
            if matches!(into, Dir::Left | Dir::Right) {
                '─'
            } else {
                '│'
            }
        }
    }
}

fn turn(route: &[(isize, isize)], index: usize) -> char {
    let here = route[index];
    let previous = index
        .checked_sub(1)
        .and_then(|prev| route.get(prev))
        .copied();
    let next = route.get(index + 1).copied();
    match (previous, next) {
        (Some(previous), Some(next)) if previous != next => {
            join(direction(previous, here), direction(here, next))
        }
        (Some(previous), _) => match direction(previous, here) {
            Dir::Up | Dir::Down => '│',
            Dir::Left | Dir::Right => '─',
        },
        // The start cell has one neighbour: the tick runs the way the route
        // leaves the border, vertical included.
        (None, Some(next)) => match direction(here, next) {
            Dir::Up | Dir::Down => '│',
            Dir::Left | Dir::Right => '─',
        },
        _ => '─',
    }
}

/// A stroke has to reach a border: when a candidate route dips through a box,
/// its cells inside are skipped and the pieces on the far side can form a
/// connected island that floats free of everything it was meant to join. Keep
/// only strokes whose connected component touches a box border — an arrowhead
/// does by construction — and blank the rest.
fn erase_islands(canvas: &mut Canvas, boxes: &[Rect]) {
    let mut visited = vec![vec![false; canvas.width]; canvas.height];
    let mut queue: Vec<(usize, usize)> = Vec::new();
    for (y, row) in canvas.cells.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            if is_stroke(*cell) && touches_box(boxes, (x as isize, y as isize)) {
                visited[y][x] = true;
                queue.push((x, y));
            }
        }
    }
    while let Some((x, y)) = queue.pop() {
        for (nx, ny) in [
            (x as isize - 1, y as isize),
            (x as isize + 1, y as isize),
            (x as isize, y as isize - 1),
            (x as isize, y as isize + 1),
        ] {
            if !canvas.holds((nx, ny)) {
                continue;
            }
            let (nx, ny) = (nx as usize, ny as usize);
            if !visited[ny][nx] && is_stroke(canvas.cells[ny][nx]) {
                visited[ny][nx] = true;
                queue.push((nx, ny));
            }
        }
    }
    for (row_cells, row_seen) in canvas.cells.iter_mut().zip(visited.iter_mut()) {
        for (cell, seen) in row_cells.iter_mut().zip(row_seen.iter_mut()) {
            if is_stroke(*cell) && !*seen {
                *cell = Cell::blank();
            }
        }
    }
}

/// Whether a cell sits beside a box: one of its four neighbours lies inside.
fn touches_box(boxes: &[Rect], cell: (isize, isize)) -> bool {
    let (x, y) = cell;
    let around = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)];
    boxes
        .iter()
        .any(|rect| around.iter().any(|point| rect.contains(*point)))
}

fn is_stroke(cell: Cell) -> bool {
    matches!(cell.kind, CellKind::Edge | CellKind::ActiveEdge)
}

/// The cells a box occupies, for the guard that keeps a stroke out of it.
fn inside_any(boxes: &[Rect], cell: (isize, isize)) -> bool {
    boxes.iter().any(|rect| rect.contains(cell))
}

/// Draw one role's box and report the title it wrote and where it starts, so
fn put(canvas: &mut Canvas, x: usize, y: usize, ch: char, kind: CellKind) {
    if y < canvas.height && x < canvas.width {
        canvas.cells[y][x] = Cell { ch, kind };
    }
}

fn put_at(canvas: &mut Canvas, cell: (isize, isize), ch: char, kind: CellKind) {
    if canvas.holds(cell) {
        put(canvas, cell.0 as usize, cell.1 as usize, ch, kind);
    }
}

fn short(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn truncate(value: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if char_len(value) <= max {
        return std::borrow::Cow::Borrowed(value);
    }
    if max == 0 {
        std::borrow::Cow::Borrowed("")
    } else if max == 1 {
        std::borrow::Cow::Borrowed("…")
    } else {
        std::borrow::Cow::Owned(format!(
            "{}…",
            value.chars().take(max - 1).collect::<String>()
        ))
    }
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> LayoutNode {
        let mut node = LayoutNode::new(name);
        node.presence = Presence::Online;
        node
    }

    fn hop(from: &str, to: &str, in_flight: bool) -> LayoutEdge {
        LayoutEdge {
            from: from.into(),
            to: to.into(),
            in_flight,
        }
    }

    /// A ring of four roles with a spoke, so the map has rings, a back hop,
    /// and a branch to route.
    fn ring() -> (Vec<LayoutNode>, Vec<LayoutEdge>) {
        let names = ["planner", "writer", "critic", "publisher"];
        let nodes = names.iter().map(|name| node(name)).collect();
        let edges = vec![
            hop("planner", "writer", true),
            hop("writer", "critic", false),
            hop("critic", "planner", false),
            hop("planner", "publisher", false),
        ];
        (nodes, edges)
    }

    fn map_of(nodes: &[LayoutNode], edges: &[LayoutEdge]) -> RoleMap {
        let mut map = RoleMap::default();
        map.sync(nodes, edges, 2);
        map
    }

    fn drawn(map: &RoleMap, nodes: &[LayoutNode], edges: &[LayoutEdge], camera: Camera) -> Canvas {
        let anchor = map.anchor(None);
        canvas(map, nodes, edges, &camera, anchor, (120, 40))
    }

    /// Each of the eight turns names the corner its arms actually form:
    /// travel order matters, `(Left,Down)` and `(Down,Left)` connect opposite
    /// pairs and must not share a glyph.
    #[test]
    fn the_eight_turns_name_their_real_corners() {
        assert_eq!(join(Dir::Left, Dir::Down), '╭');
        assert_eq!(join(Dir::Up, Dir::Right), '╭');
        assert_eq!(join(Dir::Right, Dir::Down), '╮');
        assert_eq!(join(Dir::Up, Dir::Left), '╮');
        assert_eq!(join(Dir::Right, Dir::Up), '╯');
        assert_eq!(join(Dir::Down, Dir::Left), '╯');
        assert_eq!(join(Dir::Left, Dir::Up), '╰');
        assert_eq!(join(Dir::Down, Dir::Right), '╰');
        assert_ne!(join(Dir::Left, Dir::Down), join(Dir::Down, Dir::Left));
        assert_ne!(join(Dir::Up, Dir::Right), join(Dir::Right, Dir::Up));
        assert_eq!(join(Dir::Right, Dir::Right), '─');
        assert_eq!(join(Dir::Down, Dir::Down), '│');
    }

    #[test]
    fn a_route_start_ticks_in_its_own_direction() {
        let vertical = vec![(5, 5), (5, 6), (5, 7)];
        assert_eq!(turn(&vertical, 0), '│');
        let horizontal = vec![(5, 5), (6, 5), (7, 5)];
        assert_eq!(turn(&horizontal, 0), '─');
    }

    /// The ARIS research flywheel's live graph: five roles, eleven directed
    /// hops, dense crossing. Every drawn corner is checked against the arms
    /// its neighbours actually form — the geometry, not the lookup table, is
    /// the oracle, so a mirrored table cannot pass by self-agreement.
    fn aris_graph() -> (Vec<LayoutNode>, Vec<LayoutEdge>) {
        let names = ["scout", "model", "bench", "writer", "critic"];
        let nodes = names.iter().map(|name| node(name)).collect();
        let hops: [(&str, &str); 11] = [
            ("scout", "model"),
            ("model", "bench"),
            ("model", "writer"),
            ("model", "scout"),
            ("bench", "writer"),
            ("bench", "scout"),
            ("writer", "critic"),
            ("writer", "scout"),
            ("critic", "writer"),
            ("critic", "model"),
            ("critic", "scout"),
        ];
        let edges = hops.iter().map(|(from, to)| hop(from, to, false)).collect();
        (nodes, edges)
    }

    /// Each stroke cell's neighbours say which arms it connects; the glyph
    /// must be the corner or straight that those arms name. Cells where two
    /// routes share one crossing cell (four arms) are exempt: the last writer
    /// wins there by design, and arrows are the run's own endpoints.
    #[test]
    fn every_corner_matches_the_arms_around_it() {
        let (nodes, edges) = aris_graph();
        let map = map_of(&nodes, &edges);
        let anchor = map.anchor(None);
        let canvas = canvas(&map, &nodes, &edges, &Camera::default(), anchor, (120, 40));
        let in_box = |cell: (isize, isize)| {
            canvas.node_boxes.iter().any(|b| {
                Rect {
                    x: b.x,
                    y: b.y,
                    w: b.w,
                    h: b.h,
                }
                .contains(cell)
            })
        };
        let arm_present = |cell: (isize, isize), delta: (isize, isize)| {
            let next = (cell.0 + delta.0, cell.1 + delta.1);
            if in_box(next) {
                return true;
            }
            let glyph = canvas.at(next.0, next.1).ch;
            matches!(
                glyph,
                '╭' | '╮' | '╯' | '╰' | '─' | '│' | '▸' | '◂' | '▴' | '▾'
            )
        };
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let cell = (x as isize, y as isize);
                let glyph = canvas.cells[y][x].ch;
                if !matches!(glyph, '╭' | '╮' | '╯' | '╰' | '─' | '│') {
                    continue;
                }
                if !is_stroke(canvas.cells[y][x]) || in_box(cell) {
                    continue;
                }
                let west = arm_present(cell, (-1, 0));
                let east = arm_present(cell, (1, 0));
                let north = arm_present(cell, (0, -1));
                let south = arm_present(cell, (0, 1));
                let arms = u8::from(west)
                    | u8::from(east) << 1
                    | u8::from(north) << 2
                    | u8::from(south) << 3;
                if matches!(arms, 0b1111 | 0b0000) {
                    continue;
                }
                let expected = match arms {
                    0b1010 => '╭',          // east + south
                    0b1001 => '╮',          // west + south
                    0b0101 => '╯',          // west + north
                    0b0110 => '╰',          // east + north
                    0b0011 => '─',          // west + east
                    0b1100 => '│',          // north + south
                    0b0001 | 0b0010 => '─', // single horizontal tick
                    0b0100 | 0b1000 => '│', // single vertical tick
                    _ => '╳',               // three-arm: never a legal corner
                };
                if expected == '╳' {
                    continue;
                }
                assert_eq!(
                    glyph,
                    expected,
                    "cell {cell:?} has arms w={west} e={east} n={north} s={south}, glyph {glyph:?}, expected {expected:?}\n{}",
                    canvas.text()
                );
            }
        }
    }

    /// Every drawn stroke must reach a box border through neighbouring
    /// strokes: a route lane hidden inside a box may not leave its exit
    /// fragment floating on the map. The tight view forces overlaps, so the
    /// pruning is exercised and not merely present.
    #[test]
    fn no_stroke_drifts_free_of_every_border() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let anchor = map.anchor(None);
        for view in [(120usize, 40usize), (56, 18)] {
            let canvas = canvas(&map, &nodes, &edges, &Camera::default(), anchor, view);
            let boxes: Vec<Rect> = canvas
                .node_boxes
                .iter()
                .map(|b| Rect {
                    x: b.x,
                    y: b.y,
                    w: b.w,
                    h: b.h,
                })
                .collect();
            let mut stroke = Vec::new();
            for y in 0..canvas.height {
                for x in 0..canvas.width {
                    if is_stroke(canvas.cells[y][x]) {
                        stroke.push((x as isize, y as isize));
                    }
                }
            }
            let mut live: Vec<(isize, isize)> = stroke
                .iter()
                .copied()
                .filter(|cell| touches_box(&boxes, *cell))
                .collect();
            let mut seen = live.clone();
            while let Some((x, y)) = live.pop() {
                for cell in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
                    if stroke.contains(&cell) && !seen.contains(&cell) {
                        seen.push(cell);
                        live.push(cell);
                    }
                }
            }
            for cell in &stroke {
                assert!(
                    seen.contains(cell),
                    "stroke at {cell:?} floats free of every border in view {view:?}\n{}",
                    canvas.text()
                );
            }
        }
    }

    #[test]
    fn renders_busy_star_and_aggregate_prefix() {
        let mut agg = node("supervisor");
        agg.aggregate = Some("cluster-x".into());
        agg.busy = true;
        agg.sessions.push(SessionLine {
            task: "abcdefghi".into(),
            state: SessionState::Working,
            age: "9s".into(),
        });
        let map = map_of(std::slice::from_ref(&agg), &[]);
        let text = drawn(&map, &[agg], &[], Camera::default()).text();
        assert!(text.contains("⬡supervisor*"), "{text}");
        assert!(text.contains("abcdefg"), "{text}");
        assert!(text.contains('◐'), "{text}");
    }

    #[test]
    fn the_same_topology_lays_out_the_same_map() {
        let (nodes, edges) = ring();
        let first = map_of(&nodes, &edges);
        let second = map_of(&nodes, &edges);
        assert_eq!(first.positions(), second.positions());
        assert_eq!(first.focus(), second.focus());

        // The same topology again moves nothing at all.
        let mut again = first.clone();
        assert!(
            !again.sync(&nodes, &edges, 2),
            "an unchanged map is left alone"
        );
        assert_eq!(again.positions(), first.positions());
    }

    #[test]
    fn a_new_role_lands_beside_its_peers_without_reshuffling_the_map() {
        let (nodes, edges) = ring();
        let mut map = map_of(&nodes, &edges);
        let before: BTreeMap<String, Vec2> = map.positions().clone();

        let mut grown = nodes.clone();
        grown.push(node("auditor"));
        let mut grown_edges = edges.clone();
        grown_edges.push(hop("critic", "auditor", false));
        assert!(map.sync(&grown, &grown_edges, 2), "the topology moved on");

        let ideal = map.params.ideal_len;
        for (name, was) in &before {
            let now = map.position(name).expect("a surviving role");
            assert!(
                force::span(*was, now) < ideal,
                "{name} jumped {} cells",
                force::span(*was, now)
            );
        }
        let auditor = map.position("auditor").expect("the newcomer is placed");
        let critic = map.position("critic").expect("its peer");
        assert!(
            force::span(auditor, critic) < 2.0 * ideal,
            "the newcomer seeds beside the role it talks to"
        );
        assert_eq!(map.positions().len(), grown.len());
    }

    #[test]
    fn the_focus_is_the_hub_and_the_distance_is_the_hop_count() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        assert_eq!(map.focus(), Some("planner"), "{:?}", map.focus());
        assert_eq!(map.distance("planner"), Some(0));
        assert_eq!(map.distance("writer"), Some(1));
        assert_eq!(map.distance("publisher"), Some(1));
        assert_eq!(map.distance("critic"), Some(1));
    }

    #[test]
    fn the_camera_projects_the_anchor_to_the_middle_of_the_pane() {
        let camera = Camera::default();
        let view = (100, 30);
        let anchor = Vec2::new(10.0, 10.0);
        assert_eq!(camera.project(anchor, anchor, view), (50, 15));
        assert_eq!(
            camera.project(Vec2::new(30.0, 10.0), anchor, view),
            (70, 15)
        );
        // The vertical axis packs eight tenths of a layout unit into a row, so
        // eight units down is six rows.
        assert_eq!(
            camera.project(Vec2::new(10.0, 18.0), anchor, view),
            (50, 21)
        );

        let zoomed = Camera {
            zoom: 2.0,
            pan: (0, 0),
        };
        assert_eq!(
            zoomed.project(Vec2::new(30.0, 10.0), anchor, view),
            (90, 15)
        );
        let panned = Camera {
            zoom: 1.0,
            pan: (-50, -15),
        };
        assert_eq!(panned.project(anchor, anchor, view), (0, 0));

        // A point far off the map lands outside the pane, which is how the
        // renderer decides not to draw it.
        let far = camera.project(Vec2::new(400.0, 10.0), anchor, view);
        assert!(far.0 > view.0 as isize, "{far:?}");
    }

    #[test]
    fn the_camera_stops_at_the_edge_of_the_map() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let bounds = map.extent(&nodes).expect("a map");
        let anchor = map.anchor(None);
        let view = (40, 12);

        let mut camera = Camera {
            pan: (10_000, 10_000),
            ..Camera::default()
        };
        camera.clamp_pan(bounds, anchor, view);
        assert!(camera.pan.0 < 10_000 && camera.pan.1 < 10_000, "{camera:?}");
        let edge = camera.pan;

        camera.pan = (-10_000, -10_000);
        camera.clamp_pan(bounds, anchor, view);
        assert!(
            camera.pan.0 > -10_000 && camera.pan.1 > -10_000,
            "{camera:?}"
        );
        assert!(camera.pan.0 < edge.0 && camera.pan.1 < edge.1, "{camera:?}");
        // Whatever the pan, the map stays reachable: the clamp is the map's
        // own span plus the pane, never open-ended.
        let reach = (bounds.1.x - bounds.0.x) + view.0 as f32;
        assert!((camera.pan.0.abs() as f32) <= reach + 1.0, "{camera:?}");
    }

    /// Every hop is a run of horizontal or vertical steps, and its arrowhead
    /// lands against the border of the box it enters.
    #[test]
    fn every_hop_is_orthogonal_and_lands_on_its_target() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let canvas = drawn(&map, &nodes, &edges, Camera::default());
        let text = canvas.text();
        assert_eq!(canvas.edge_paths.len(), edges.len(), "{text}");
        for edge in &edges {
            let path = canvas
                .edge_paths
                .get(&(edge.from.clone(), edge.to.clone()))
                .unwrap_or_else(|| panic!("no path for {}→{}\n{text}", edge.from, edge.to));
            for step in path.windows(2) {
                assert!(
                    step[0].0 == step[1].0 || step[0].1 == step[1].1,
                    "{}→{} steps diagonally at {step:?}\n{text}",
                    edge.from,
                    edge.to
                );
                assert!(
                    step[0].0.abs_diff(step[1].0) <= 1 && step[0].1.abs_diff(step[1].1) <= 1,
                    "{}→{} jumps at {step:?}\n{text}",
                    edge.from,
                    edge.to
                );
            }
            let target = canvas
                .node_boxes
                .iter()
                .find(|rect| rect.name == edge.to)
                .expect("the target box");
            let tip = *path.last().expect("a route");
            let glyph = canvas.at(tip.0, tip.1).ch;
            assert!(
                ['▸', '◂', '▴', '▾'].contains(&glyph),
                "{}→{} ends on {glyph:?}, not an arrowhead\n{text}",
                edge.from,
                edge.to
            );
            // The arrow cell points the way the last run travels, and it
            // touches the border it enters.
            let previous = path[path.len() - 2];
            let approach = direction(previous, tip);
            let expected = match approach {
                Dir::Right => '▸',
                Dir::Left => '◂',
                Dir::Down => '▾',
                Dir::Up => '▴',
            };
            assert_eq!(glyph, expected, "{}→{}\n{text}", edge.from, edge.to);
            let target_rect = Rect {
                x: target.x,
                y: target.y,
                w: target.w,
                h: target.h,
            };
            assert!(!target_rect.contains(tip), "the arrow sits inside the box");
            assert!(
                [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| target_rect.contains((tip.0 + dx, tip.1 + dy))),
                "{}→{} ends at {tip:?}, not against {target:?}\n{text}",
                edge.from,
                edge.to
            );
        }
    }

    #[test]
    fn a_hop_never_enters_a_box() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let canvas = drawn(&map, &nodes, &edges, Camera::default());
        for rect in &canvas.node_boxes {
            for cell in canvas.edge_paths.values().flatten() {
                assert!(
                    cell.0 < rect.x
                        || cell.0 >= rect.x + rect.w
                        || cell.1 < rect.y
                        || cell.1 >= rect.y + rect.h,
                    "a hop enters {rect:?} at {cell:?}\n{}",
                    canvas.text()
                );
            }
        }
    }

    #[test]
    fn the_map_holds_no_diagonal_stroke() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let text = drawn(&map, &nodes, &edges, Camera::default()).text();
        for banned in ['╱', '╲', '╳', '/', '\\'] {
            assert!(!text.contains(banned), "{banned:?} in\n{text}");
        }
    }

    #[test]
    fn an_in_flight_hop_is_stroked_active_and_points_at_its_target() {
        let (nodes, edges) = ring();
        let map = map_of(&nodes, &edges);
        let canvas = drawn(&map, &nodes, &edges, Camera::default());
        assert!(
            canvas
                .cells
                .iter()
                .flatten()
                .any(|cell| cell.kind == CellKind::ActiveEdge),
            "{}",
            canvas.text()
        );
    }

    #[test]
    fn boxes_never_overlap_however_dense_the_topology() {
        let nodes: Vec<LayoutNode> = (0..9).map(|i| node(&format!("role{i}"))).collect();
        let mut edges = Vec::new();
        for i in 0..9 {
            for j in i + 1..9 {
                edges.push(hop(&format!("role{i}"), &format!("role{j}"), false));
            }
        }
        let map = map_of(&nodes, &edges);
        let anchor = map.anchor(None);
        let canvas = canvas(&map, &nodes, &edges, &Camera::default(), anchor, (400, 200));
        let text = canvas.text();
        assert_eq!(canvas.node_boxes.len(), nodes.len(), "{text}");
        for (index, rect) in canvas.node_boxes.iter().enumerate() {
            for other in &canvas.node_boxes[index + 1..] {
                assert!(
                    rect.x + rect.w <= other.x
                        || other.x + other.w <= rect.x
                        || rect.y + rect.h <= other.y
                        || other.y + other.h <= rect.y,
                    "{rect:?} sits on {other:?}\n{text}"
                );
            }
        }
    }

    #[test]
    fn zooming_in_drops_the_roles_past_the_tier() {
        let nodes: Vec<LayoutNode> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| node(name))
            .collect();
        let edges = vec![
            hop("a", "b", false),
            hop("b", "c", false),
            hop("c", "d", false),
            hop("d", "e", false),
        ];
        let map = map_of(&nodes, &edges);
        let anchor = map.anchor(None);
        let overview = canvas(&map, &nodes, &edges, &Camera::default(), anchor, (400, 200));
        assert_eq!(overview.node_boxes.len(), nodes.len());

        let close = drawn(
            &map,
            &nodes,
            &edges,
            Camera {
                zoom: MAX_ZOOM,
                pan: (0, 0),
            },
        );
        assert!(
            close.node_boxes.len() < nodes.len(),
            "the focus+ring tier hides the far end\n{}",
            close.text()
        );
        assert!(
            close
                .node_boxes
                .iter()
                .any(|rect| rect.name == map.focus().expect("a focus")),
            "the focus itself stays"
        );
    }

    #[test]
    fn a_hop_route_prefers_a_free_lane() {
        // Two boxes side by side with a third directly between them: the route
        // has to step around it.
        let from = Rect {
            x: 0,
            y: 10,
            w: 10,
            h: 5,
        };
        let to = Rect {
            x: 40,
            y: 10,
            w: 10,
            h: 5,
        };
        let blocker = Rect {
            x: 20,
            y: 8,
            w: 10,
            h: 9,
        };
        let route = route(from, to, &[from, to, blocker]);
        assert!(!route.is_empty());
        assert!(
            !route.iter().any(|cell| blocker.contains(*cell)),
            "{route:?} crosses the blocker"
        );
        // Still an orthogonal path with at most two corners.
        let corners = route
            .windows(3)
            .filter(|pair| {
                let a = direction(pair[0], pair[1]);
                let b = direction(pair[1], pair[2]);
                a != b
            })
            .count();
        assert!(corners <= 2, "{corners} corners in {route:?}");
    }
}
