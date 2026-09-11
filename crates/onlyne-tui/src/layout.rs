use crate::model::{RoleSlot, control_role, ring_places, role_positions};
use std::collections::BTreeMap;

/// The widest a role box draws. A box's own text only asks for what it needs,
/// so this is the ceiling the world size starts from.
const NODE_W: usize = 28;
/// The narrowest box that still shows a title and a session line.
const NODE_MIN_W: usize = 14;
/// Box height.
const NODE_H: usize = 7;
/// The gap the grid leaves between two boxes.
const GRID_GAP: usize = 2;

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
    /// The cells each routed hop drew, keyed by `(from, to)`. The map paints
    /// the hop `j`/`k` highlights from these coordinates.
    pub edge_paths: BTreeMap<(String, String), Vec<(usize, usize)>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeBox {
    pub name: String,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// The text drawn on the box's title row and the column it starts at. The
    /// placement never fills them; the drawing pass does.
    pub label: String,
    pub label_x: usize,
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
    /// as blank, so the pane can crop the world wherever the camera sits.
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
/// between the width that still reads and the width the ring has room for.
pub fn box_width(nodes: &[LayoutNode]) -> usize {
    let longest = nodes.iter().map(longest_line).max().unwrap_or(8);
    (longest + 4).clamp(NODE_MIN_W, NODE_W)
}

fn longest_line(node: &LayoutNode) -> usize {
    let mut longest =
        node.title.chars().count() + usize::from(node.aggregate.is_some()) + usize::from(node.busy);
    for session in node.sessions.iter().take(4) {
        longest =
            longest.max(session.task.chars().count().min(8) + session.age.chars().count() + 4);
    }
    if node.sessions.len() > 4 {
        longest = longest.max(3);
    }
    longest
}

fn slots_of(nodes: &[LayoutNode]) -> Vec<RoleSlot> {
    nodes
        .iter()
        .map(|node| RoleSlot {
            name: node.name.clone(),
            control: control_role(&node.name, node.aggregate.as_deref()),
        })
        .collect()
}

/// The smallest world that holds the map without two boxes touching, and
/// without the control plane landing on top of the ring.
///
/// A cycle keeps its ring: the map asks for the smallest world the ellipse and
/// its anchor fit in, and only a map that cannot draw one falls back to the
/// serpentine grid.
pub fn world_size(nodes: &[LayoutNode]) -> (usize, usize) {
    let slots = slots_of(nodes);
    let node_w = box_width(nodes);
    let cycle = slots.iter().filter(|slot| !slot.control).count();
    if cycle >= 3 {
        if let Some(world) = ring_world(&slots, node_w) {
            return world;
        }
    }
    grid_world(slots.len(), node_w)
}

/// The world the ring reads from: the smallest area the ellipse fits in with a
/// free anchor, then the narrowest of those.
fn ring_world(slots: &[RoleSlot], node_w: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut width = node_w * 3;
    while width <= node_w * 4 + 8 {
        let mut height = NODE_H * 3 + 4;
        while height <= NODE_H * 6 {
            if ring_places(slots, width, height, node_w, NODE_H).is_some() {
                let area = width * height;
                let current = best.map(|(w, h)| w * h);
                if current.map(|current| area < current).unwrap_or(true)
                    || current == Some(area) && best.map(|(w, _)| width < w).unwrap_or(false)
                {
                    best = Some((width, height));
                }
            }
            height += 2;
        }
        width += 2;
    }
    best
}

/// The serpentine grid world: `per_row` boxes to a row, wrapped as needed.
fn grid_world(count: usize, node_w: usize) -> (usize, usize) {
    if count == 0 {
        return (node_w, NODE_H);
    }
    let per_row = count_columns(count);
    let rows = count.div_ceil(per_row).max(1);
    (
        node_w * per_row + GRID_GAP * (per_row - 1),
        NODE_H * rows + GRID_GAP * (rows - 1),
    )
}

/// The columns a grid world uses: enough to keep the boxes from stacking into
/// one tall column.
fn count_columns(count: usize) -> usize {
    let mut columns = 1;
    while columns * columns < count {
        columns += 1;
    }
    columns
}

/// Draw the role map: the ring the ACL forms, one box per role, one segment
/// per hop.
pub fn layout(nodes: &[LayoutNode], edges: &[LayoutEdge], w: u16, h: u16) -> Canvas {
    let width = w as usize;
    let height = h as usize;
    let mut canvas = Canvas::new(width, height);
    if width < 12 || height < 4 || nodes.is_empty() {
        draw_text(&mut canvas, 0, 0, "(no roles)", CellKind::Plain);
        return canvas;
    }
    let node_w = box_width(nodes);
    let node_h = NODE_H.min(height.max(4));
    let slots = slots_of(nodes);
    let mut boxes = BTreeMap::new();
    for (name, place) in role_positions(&slots, width, height, node_w, node_h) {
        boxes.insert(
            name.clone(),
            NodeBox {
                name,
                x: place.x,
                y: place.y,
                w: node_w,
                h: node_h,
                ..NodeBox::default()
            },
        );
    }
    // Hops first, so a box border stays on top of every line that reaches it.
    for edge in edges {
        draw_edge(&mut canvas, &boxes, edge);
    }
    for node in nodes {
        if let Some(rect) = boxes.get(&node.name) {
            let (label, label_x) = draw_node(&mut canvas, node, rect);
            canvas.node_boxes.push(NodeBox {
                label,
                label_x,
                ..rect.clone()
            });
        }
    }
    canvas.node_boxes.sort_by(|a, b| a.name.cmp(&b.name));
    canvas
}

/// The rect the map draws one role's box at inside `world`.
pub fn role_box(nodes: &[LayoutNode], name: &str, world: (usize, usize)) -> Option<NodeBox> {
    let slots = slots_of(nodes);
    let node_w = box_width(nodes);
    let node_h = NODE_H.min(world.1.max(4));
    role_positions(&slots, world.0, world.1, node_w, node_h)
        .into_iter()
        .find(|(slot, _)| slot == name)
        .map(|(_, place)| NodeBox {
            name: name.to_string(),
            x: place.x,
            y: place.y,
            w: node_w,
            h: node_h,
            ..NodeBox::default()
        })
}

/// One hop: a straight segment between the two boxes' facing borders, with an
/// arrowhead on the target end.
fn draw_edge(canvas: &mut Canvas, boxes: &BTreeMap<String, NodeBox>, edge: &LayoutEdge) {
    let (Some(from), Some(to)) = (boxes.get(&edge.from), boxes.get(&edge.to)) else {
        return;
    };
    let kind = if edge.in_flight {
        CellKind::ActiveEdge
    } else {
        CellKind::Edge
    };
    let start = anchor(from, center(to));
    let end = anchor(to, center(from));
    let mut path = Vec::new();
    stroke(canvas, boxes, start, end, kind, &mut path);
    if let Some(tip) = path.last().copied() {
        put(canvas, tip.0, tip.1, '▶', kind);
    }
    canvas
        .edge_paths
        .insert((edge.from.clone(), edge.to.clone()), path);
}

/// The middle of a box.
fn center(rect: &NodeBox) -> (i64, i64) {
    ((rect.x + rect.w / 2) as i64, (rect.y + rect.h / 2) as i64)
}

/// The border cell a line from the box's middle toward `toward` leaves by.
fn anchor(rect: &NodeBox, toward: (i64, i64)) -> (usize, usize) {
    let centre = center(rect);
    let dx = (toward.0 - centre.0) as f64;
    let dy = (toward.1 - centre.1) as f64;
    let tx = if dx.abs() > f64::EPSILON {
        (rect.w as f64 / 2.0) / dx.abs()
    } else {
        f64::INFINITY
    };
    let ty = if dy.abs() > f64::EPSILON {
        (rect.h as f64 / 2.0) / dy.abs()
    } else {
        f64::INFINITY
    };
    let t = tx.min(ty);
    let x = (centre.0 as f64 + dx * t).round() as i64;
    let y = (centre.1 as f64 + dy * t).round() as i64;
    (
        x.clamp(rect.x as i64, (rect.x + rect.w - 1) as i64) as usize,
        y.clamp(rect.y as i64, (rect.y + rect.h - 1) as i64) as usize,
    )
}

/// Stroke a segment, skipping the inside of every box so no hop writes over a
/// border or a title.
fn stroke(
    canvas: &mut Canvas,
    boxes: &BTreeMap<String, NodeBox>,
    start: (usize, usize),
    end: (usize, usize),
    kind: CellKind,
    path: &mut Vec<(usize, usize)>,
) {
    let mut previous: Option<(usize, usize)> = None;
    for cell in line(start, end) {
        if inside_any_box(boxes, cell) {
            previous = None;
            continue;
        }
        let ch = stroke_char(previous.unwrap_or(start), cell);
        put(canvas, cell.0, cell.1, ch, kind);
        path.push(cell);
        previous = Some(cell);
    }
}

/// The line char one step of `to - from` draws.
fn stroke_char(from: (usize, usize), to: (usize, usize)) -> char {
    let dx = to.0 as isize - from.0 as isize;
    let dy = to.1 as isize - from.1 as isize;
    match (dx.signum(), dy.signum()) {
        (0, _) => '│',
        (_, 0) => '─',
        (1, -1) | (-1, 1) => '╱',
        _ => '╲',
    }
}

fn inside_any_box(boxes: &BTreeMap<String, NodeBox>, cell: (usize, usize)) -> bool {
    boxes.values().any(|rect| {
        cell.0 >= rect.x && cell.0 < rect.x + rect.w && cell.1 >= rect.y && cell.1 < rect.y + rect.h
    })
}

/// Every cell on the segment, start and end included.
fn line(start: (usize, usize), end: (usize, usize)) -> Vec<(usize, usize)> {
    let (x1, y1) = (end.0 as isize, end.1 as isize);
    let (mut x, mut y) = (start.0 as isize, start.1 as isize);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut error = dx + dy;
    let mut cells = Vec::new();
    loop {
        cells.push((x.max(0) as usize, y.max(0) as usize));
        if x == x1 && y == y1 {
            break;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += sx;
        }
        if twice <= dx {
            error += dx;
            y += sy;
        }
    }
    cells
}

/// Draw one role's box and report the title it wrote and where it starts, so
/// the pane can reverse the cursor's label on top.
fn draw_node(canvas: &mut Canvas, node: &LayoutNode, rect: &NodeBox) -> (String, usize) {
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
            put(canvas, x, y, ' ', CellKind::Plain);
        }
    }
    put(canvas, rect.x, rect.y, '╭', border);
    for x in rect.x + 1..rect.x + rect.w - 1 {
        put(canvas, x, rect.y, '─', border);
    }
    put(canvas, rect.x + rect.w - 1, rect.y, '╮', border);
    for y in rect.y + 1..rect.y + rect.h - 1 {
        put(canvas, rect.x, y, '│', border);
        put(canvas, rect.x + rect.w - 1, y, '│', border);
    }
    put(canvas, rect.x, rect.y + rect.h - 1, '╰', border);
    for x in rect.x + 1..rect.x + rect.w - 1 {
        put(canvas, x, rect.y + rect.h - 1, '─', border);
    }
    put(
        canvas,
        rect.x + rect.w - 1,
        rect.y + rect.h - 1,
        '╯',
        border,
    );

    let mut title = String::new();
    if node.aggregate.is_some() {
        title.push('⬡');
    }
    title.push_str(&node.title);
    let available = rect.w.saturating_sub(4);
    let title = truncate(&title, available.saturating_sub(usize::from(node.busy)));
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
        let star_x = label_x + char_len(&title);
        if star_x < rect.x + rect.w - 1 {
            put(canvas, star_x, rect.y, '*', CellKind::Busy);
        }
    }

    let visible = node.sessions.iter().take(4).collect::<Vec<_>>();
    for (idx, session) in visible.iter().enumerate() {
        let text = format!(
            "{} {} {}",
            short(&session.task, 8),
            session.state.glyph(),
            session.age
        );
        draw_text(
            canvas,
            rect.y + 1 + idx,
            label_x,
            &truncate(&text, rect.w.saturating_sub(4)),
            CellKind::Plain,
        );
    }
    if node.sessions.len() > 4 && rect.h > 6 {
        let text = format!("+{}", node.sessions.len() - 4);
        draw_text(canvas, rect.y + 5, label_x, &text, CellKind::Plain);
    }
    (title.into_owned(), label_x)
}

fn draw_text(canvas: &mut Canvas, y: usize, x: usize, text: &str, kind: CellKind) {
    for (idx, ch) in text.chars().enumerate() {
        put(canvas, x + idx, y, ch, kind);
    }
}

fn put(canvas: &mut Canvas, x: usize, y: usize, ch: char, kind: CellKind) {
    if y < canvas.height && x < canvas.width {
        canvas.cells[y][x] = Cell { ch, kind };
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
        let canvas = layout(&[agg], &[], 40, 10);
        let text = canvas.text();
        assert!(text.contains("⬡supervisor*"), "{text}");
        assert!(text.contains("abcdefg"), "{text}");
        assert!(text.contains('◐'), "{text}");
    }

    #[test]
    fn the_ring_draws_every_box_without_overlap() {
        let names = ["a", "b", "c", "d", "e", "_supervisor"];
        let nodes: Vec<LayoutNode> = names.iter().map(|name| node(name)).collect();
        let world = world_size(&nodes);
        let canvas = layout(&nodes, &[], world.0 as u16, world.1 as u16);
        assert_eq!(canvas.node_boxes.len(), names.len(), "{world:?}");
        for (index, rect) in canvas.node_boxes.iter().enumerate() {
            assert!(
                rect.x + rect.w <= world.0 && rect.y + rect.h <= world.1,
                "{rect:?} leaves {world:?}"
            );
            for other in &canvas.node_boxes[index + 1..] {
                assert!(
                    rect.x + rect.w <= other.x
                        || other.x + other.w <= rect.x
                        || rect.y + rect.h <= other.y
                        || other.y + other.h <= rect.y,
                    "{rect:?} sits on {other:?}"
                );
            }
        }
        let text = canvas.text();
        for name in names {
            assert!(text.contains(&format!("╭─{name}")), "{text}");
        }
        let rows: std::collections::BTreeSet<usize> = canvas
            .node_boxes
            .iter()
            .filter(|rect| rect.name != "_supervisor")
            .map(|rect| rect.y)
            .collect();
        assert!(rows.len() >= 3, "the ring spreads over rows\n{text}");
    }

    #[test]
    fn an_in_flight_hop_is_stroked_active_and_points_at_its_target() {
        let nodes = vec![node("a"), node("b")];
        let edges = vec![hop("a", "b", true)];
        let world = world_size(&nodes);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16);
        let text = canvas.text();
        assert!(
            canvas
                .cells
                .iter()
                .flatten()
                .any(|cell| cell.kind == CellKind::ActiveEdge),
            "{text}"
        );
        assert!(text.contains('▶'), "{text}");
        assert!(
            canvas
                .edge_paths
                .contains_key(&("a".to_string(), "b".to_string())),
            "{text}"
        );
    }

    #[test]
    fn a_hop_never_enters_a_box() {
        let nodes = vec![node("a"), node("b")];
        let edges = vec![hop("a", "b", true)];
        let world = world_size(&nodes);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16);
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
    fn the_same_input_draws_the_same_map() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![hop("a", "b", false), hop("b", "c", false)];
        let world = world_size(&nodes);
        let first = layout(&nodes, &edges, world.0 as u16, world.1 as u16);
        let second = layout(&nodes, &edges, world.0 as u16, world.1 as u16);
        assert_eq!(first.text(), second.text());
        assert_eq!(first.node_boxes, second.node_boxes);
    }

    #[test]
    fn role_box_finds_the_corner_the_map_drew() {
        let nodes = vec![node("a"), node("b")];
        let world = world_size(&nodes);
        let canvas = layout(&nodes, &[], world.0 as u16, world.1 as u16);
        let drawn = canvas
            .node_boxes
            .iter()
            .find(|rect| rect.name == "b")
            .expect("the box");
        let placed = role_box(&nodes, "b", world).expect("the same box");
        assert_eq!(
            (placed.x, placed.y, placed.w, placed.h),
            (drawn.x, drawn.y, drawn.w, drawn.h)
        );
    }
}
