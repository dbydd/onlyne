use crate::model::{
    PLACE_MARGIN, RoleSlot, clamp_spacing, control_role, role_positions, spacing_gap,
};
use std::collections::BTreeMap;

/// The widest a role box draws. A box's own text only asks for what it needs,
/// so this is the ceiling the world size starts from.
const NODE_W: usize = 28;
/// The narrowest box that still shows a title and a session line.
const NODE_MIN_W: usize = 14;
/// Box height.
const NODE_H: usize = 7;

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
/// between the width that still reads and the width the grid has room for.
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

fn rank_from_x(x: usize, node_w: usize, gap: usize) -> usize {
    x.saturating_sub(PLACE_MARGIN) / (node_w + gap).max(1)
}

fn rank_of(rect: &NodeBox, spacing: usize) -> usize {
    rank_from_x(rect.x, rect.w, spacing_gap(spacing))
}

/// The world the layered map draws in, plus an outer gutter for return edges.
pub fn world_size(nodes: &[LayoutNode], edges: &[LayoutEdge], spacing: usize) -> (usize, usize) {
    let slots = slots_of(nodes);
    let node_w = box_width(nodes);
    let gap = spacing_gap(spacing);
    if slots.is_empty() {
        return (2 * PLACE_MARGIN + node_w + gap, NODE_H + gap);
    }
    let places = role_positions(&slots, edges, usize::MAX, node_w, NODE_H, spacing);
    let mut rows_by_rank = BTreeMap::<usize, usize>::new();
    let mut max_rank = 0;
    for (_, place) in places {
        let rank = rank_from_x(place.x, node_w, gap);
        max_rank = max_rank.max(rank);
        *rows_by_rank.entry(rank).or_default() += 1;
    }
    let columns = max_rank + 1;
    let rows = rows_by_rank.values().copied().max().unwrap_or(1).max(1);
    (
        2 * PLACE_MARGIN + columns * node_w + columns * gap,
        rows * (NODE_H + gap),
    )
}

/// Draw the role map: rank columns left to right, with routed ACL hops.
pub fn layout(
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    w: u16,
    h: u16,
    spacing: usize,
) -> Canvas {
    let width = w as usize;
    let height = h as usize;
    let spacing = clamp_spacing(spacing);
    let mut canvas = Canvas::new(width, height);
    if width < 12 || height < 4 || nodes.is_empty() {
        draw_text(&mut canvas, 0, 0, "(no roles)", CellKind::Plain);
        return canvas;
    }
    let node_w = box_width(nodes);
    let node_h = NODE_H.min(height.max(4));
    let slots = slots_of(nodes);
    let mut boxes = BTreeMap::new();
    for (name, place) in role_positions(&slots, edges, width, node_w, node_h, spacing) {
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
    // Hops first, so a box border stays on top of every line that reaches it;
    // their arrowheads last, so two hops sharing a gutter cannot erase the
    // other's direction.
    let mut tips = Vec::new();
    let mut lanes = BTreeMap::new();
    let bottom_y = boxes
        .values()
        .map(|rect| rect.y + rect.h)
        .max()
        .unwrap_or(0)
        .min(height.saturating_sub(1));
    for edge in edges {
        draw_edge(
            &mut canvas,
            &boxes,
            edge,
            spacing,
            bottom_y,
            &mut lanes,
            &mut tips,
        );
    }
    for (cell, ch, kind) in tips {
        put(&mut canvas, cell.0, cell.1, ch, kind);
    }
    erase_strays(&mut canvas);
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
pub fn role_box(
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    name: &str,
    world: (usize, usize),
    spacing: usize,
) -> Option<NodeBox> {
    let slots = slots_of(nodes);
    let node_w = box_width(nodes);
    let node_h = NODE_H.min(world.1.max(4));
    role_positions(&slots, edges, world.0, node_w, node_h, spacing)
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

/// One hop: an orthogonal route leaves a box's right border, runs in gutters,
/// and enters the target from its left border. No path cell enters a box.
fn draw_edge(
    canvas: &mut Canvas,
    boxes: &BTreeMap<String, NodeBox>,
    edge: &LayoutEdge,
    spacing: usize,
    bottom_y: usize,
    lanes: &mut BTreeMap<usize, usize>,
    tips: &mut Vec<((usize, usize), char, CellKind)>,
) {
    let (Some(from), Some(to)) = (boxes.get(&edge.from), boxes.get(&edge.to)) else {
        return;
    };
    let kind = if edge.in_flight {
        CellKind::ActiveEdge
    } else {
        CellKind::Edge
    };
    let back = rank_of(to, spacing) <= rank_of(from, spacing);
    let lane_key = if back {
        usize::MAX
    } else {
        rank_of(from, spacing)
    };
    let next_lane = lanes.entry(lane_key).or_default();
    let gap = spacing_gap(spacing).max(1);
    let geo = Geo {
        world_w: canvas.width,
        world_h: canvas.height,
        gap,
        lane: *next_lane % gap,
    };
    *next_lane += 1;
    let route = route(from, to, &geo, bottom_y, back);
    let mut path = Vec::new();
    for (index, cell) in route.iter().enumerate() {
        if inside_any_box(boxes, *cell) {
            continue;
        }
        let ch = if index + 1 == route.len() {
            ' '
        } else {
            turn(&route, index)
        };
        if index + 1 == route.len() {
            tips.push((*cell, arrowhead(to, *cell), kind));
        }
        put(canvas, cell.0, cell.1, ch, kind);
        path.push(*cell);
    }
    canvas
        .edge_paths
        .insert((edge.from.clone(), edge.to.clone()), path);
}

/// The canvas geometry one hop routes against: world size plus the lane this
/// edge runs in inside the gap. Built once per draw_edge call.
#[derive(Clone, Copy)]
struct Geo {
    world_w: usize,
    world_h: usize,
    gap: usize,
    lane: usize,
}

/// The cells a hop's path takes, from the source's right gutter to the
/// target's left gutter. Back edges use the bottom corridor.
fn route(
    from: &NodeBox,
    to: &NodeBox,
    geo: &Geo,
    bottom_y: usize,
    back: bool,
) -> Vec<(usize, usize)> {
    if geo.world_w == 0 || geo.world_h == 0 {
        return Vec::new();
    }
    let start = (
        (from.x + from.w).min(geo.world_w.saturating_sub(1)),
        (from.y + from.h / 2).min(geo.world_h.saturating_sub(1)),
    );
    let tip = (
        to.x.saturating_sub(1).min(geo.world_w.saturating_sub(1)),
        (to.y + to.h / 2).min(geo.world_h.saturating_sub(1)),
    );
    if back {
        route_back(from, start, tip, geo, bottom_y)
    } else {
        route_forward(from, to, start, tip, geo)
    }
}

fn route_forward(
    from: &NodeBox,
    to: &NodeBox,
    start: (usize, usize),
    tip: (usize, usize),
    geo: &Geo,
) -> Vec<(usize, usize)> {
    let source_lane_x = (from.x + from.w + geo.lane).min(geo.world_w.saturating_sub(1));
    let corridor_y = (from.y + from.h)
        .max(to.y + to.h)
        .saturating_add(geo.lane.min(geo.gap.saturating_sub(1)))
        .min(geo.world_h.saturating_sub(1));
    let mut points = vec![start];
    push(&mut points, (source_lane_x, start.1));
    push(&mut points, (source_lane_x, corridor_y));
    push(&mut points, (tip.0, corridor_y));
    push(&mut points, tip);
    expand(&points)
}

fn route_back(
    from: &NodeBox,
    start: (usize, usize),
    tip: (usize, usize),
    geo: &Geo,
    bottom_y: usize,
) -> Vec<(usize, usize)> {
    let source_lane_x = (from.x + from.w + geo.lane).min(geo.world_w.saturating_sub(1));
    let source_corridor_y = (from.y + from.h)
        .saturating_add(geo.lane.min(geo.gap.saturating_sub(1)))
        .min(geo.world_h.saturating_sub(1));
    let outer_x = geo
        .world_w
        .saturating_sub(PLACE_MARGIN + geo.gap)
        .saturating_add(geo.lane)
        .min(geo.world_w.saturating_sub(1));
    let bottom_y = bottom_y
        .saturating_add(geo.lane.min(geo.gap.saturating_sub(1)))
        .min(geo.world_h.saturating_sub(1));
    let mut points = vec![start];
    push(&mut points, (source_lane_x, start.1));
    push(&mut points, (source_lane_x, source_corridor_y));
    push(&mut points, (outer_x, source_corridor_y));
    push(&mut points, (outer_x, bottom_y));
    push(&mut points, (tip.0, bottom_y));
    push(&mut points, tip);
    expand(&points)
}

/// Add an axis-aligned step to `next`, turning once when both axes differ.
fn push(points: &mut Vec<(usize, usize)>, next: (usize, usize)) {
    let Some(last) = points.last().copied() else {
        points.push(next);
        return;
    };
    if last == next {
        return;
    }
    if last.0 != next.0 && last.1 != next.1 {
        points.push((last.0, next.1));
    }
    points.push(next);
}

/// Walk the waypoints in order, so the cells come back in the order the hop
/// travels: source first, target last.
fn expand(points: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut cells: Vec<(usize, usize)> = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.1 == b.1 {
            let step = if b.0 >= a.0 { 1isize } else { -1 };
            let mut x = a.0 as isize;
            while x != b.0 as isize + step {
                push_cell(&mut cells, (x.max(0) as usize, a.1));
                x += step;
            }
        } else {
            let step = if b.1 >= a.1 { 1isize } else { -1 };
            let mut y = a.1 as isize;
            while y != b.1 as isize + step {
                push_cell(&mut cells, (a.0, y.max(0) as usize));
                y += step;
            }
        }
    }
    if cells.is_empty() {
        cells.extend(points.iter().copied());
    }
    cells
}

fn push_cell(cells: &mut Vec<(usize, usize)>, cell: (usize, usize)) {
    if cells.last().copied() != Some(cell) {
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

fn direction(from: (usize, usize), to: (usize, usize)) -> Dir {
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

/// The corner glyph where two runs meet.
fn join(from: Dir, to: Dir) -> char {
    match (from, to) {
        (Dir::Right, Dir::Down) | (Dir::Up, Dir::Left) => '╗',
        (Dir::Left, Dir::Down) | (Dir::Up, Dir::Right) => '╔',
        (Dir::Right, Dir::Up) | (Dir::Down, Dir::Left) => '╝',
        (Dir::Left, Dir::Up) | (Dir::Down, Dir::Right) => '╚',
        (Dir::Up, Dir::Up | Dir::Down) | (Dir::Down, Dir::Up | Dir::Down) => '│',
        _ => '─',
    }
}

/// A stroke cell with nothing beside it would float free of every box and
/// gutter; drop it, so each drawn cell either touches a border or lies on a
/// run. Arrowheads stay: they are the point of the drawing.
fn erase_strays(canvas: &mut Canvas) {
    let mut stray = Vec::new();
    for y in 0..canvas.height {
        for x in 0..canvas.width {
            if !is_stroke(canvas.cells[y][x]) {
                continue;
            }
            let linked = [
                (x as isize - 1, y as isize),
                (x as isize + 1, y as isize),
                (x as isize, y as isize - 1),
                (x as isize, y as isize + 1),
            ]
            .into_iter()
            .any(|(nx, ny)| is_stroke(canvas.at(nx, ny)));
            let arrow = matches!(canvas.cells[y][x].ch, '▶' | '◀' | '▲' | '▼');
            if !linked && !arrow {
                stray.push((x, y));
            }
        }
    }
    for (x, y) in stray {
        canvas.cells[y][x] = Cell::blank();
    }
}

fn is_stroke(cell: Cell) -> bool {
    matches!(cell.kind, CellKind::Edge | CellKind::ActiveEdge)
}
fn turn(route: &[(usize, usize)], index: usize) -> char {
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
        _ => '─',
    }
}

/// The arrowhead for the cell against `target`'s border: which border the cell
/// touches decides which way it points.
fn arrowhead(target: &NodeBox, cell: (usize, usize)) -> char {
    if cell.1 + 1 == target.y {
        '▼'
    } else if cell.1 == target.y + target.h {
        '▲'
    } else if cell.0 + 1 == target.x {
        '▶'
    } else if cell.0 == target.x + target.w {
        '◀'
    } else {
        '▶'
    }
}

/// The cells a box occupies, for the guard that keeps a stroke out of it.
fn inside_any_box(boxes: &BTreeMap<String, NodeBox>, cell: (usize, usize)) -> bool {
    boxes.values().any(|rect| {
        cell.0 >= rect.x && cell.0 < rect.x + rect.w && cell.1 >= rect.y && cell.1 < rect.y + rect.h
    })
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
        let canvas = layout(&[agg], &[], 40, 10, 2);
        let text = canvas.text();
        assert!(text.contains("⬡supervisor*"), "{text}");
        assert!(text.contains("abcdefg"), "{text}");
        assert!(text.contains('◐'), "{text}");
    }

    #[test]
    fn layered_map_draws_every_box_without_overlap() {
        let names = ["a", "b", "c", "d", "e", "_supervisor"];
        let nodes: Vec<LayoutNode> = names.iter().map(|name| node(name)).collect();
        let edges = Vec::new();
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
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
        let at = |name: &str| {
            canvas
                .node_boxes
                .iter()
                .find(|rect| rect.name == name)
                .unwrap_or_else(|| panic!("no {name}"))
        };
        assert_eq!(at("_supervisor").x, PLACE_MARGIN, "{text}");
        for name in ["a", "b", "c", "d", "e"] {
            assert!(at(name).x > at("_supervisor").x, "{text}");
        }
        assert_eq!(at("a").x, at("e").x, "{text}");
        assert!(at("e").y > at("a").y, "{text}");
    }

    /// Every hop is a run of horizontal or vertical steps, and its arrowhead
    /// lands against the target's border: the shape the operator asked for.
    #[test]
    fn every_hop_runs_orthogonally_and_lands_on_its_target() {
        let names = ["a", "b", "c", "d", "e", "_supervisor"];
        let nodes: Vec<LayoutNode> = names.iter().map(|name| node(name)).collect();
        let edges = vec![
            hop("a", "b", true),
            hop("b", "c", false),
            hop("c", "d", true),
            hop("d", "e", false),
            hop("e", "a", false),
            hop("_supervisor", "c", false),
            hop("a", "d", true),
        ];
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
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
            assert!(
                ['▶', '◀', '▲', '▼'].contains(&canvas.cells[tip.1][tip.0].ch),
                "{}→{} ends on {:?}, not an arrowhead\n{text}",
                edge.from,
                edge.to,
                canvas.cells[tip.1][tip.0].ch
            );
            assert!(
                tip.0 + 1 == target.x,
                "{}→{} ends at {tip:?}, not in {}'s left gutter {target:?}\n{text}",
                edge.from,
                edge.to,
                edge.to
            );
        }
    }

    #[test]
    fn an_in_flight_hop_is_stroked_active_and_points_at_its_target() {
        let nodes = vec![node("a"), node("b")];
        let edges = vec![hop("a", "b", true)];
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
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
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
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
    fn back_edge_uses_bottom_corridor_without_crossing_boxes() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![
            hop("a", "b", false),
            hop("b", "c", false),
            hop("c", "a", false),
        ];
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
        let path = canvas
            .edge_paths
            .get(&("c".to_string(), "a".to_string()))
            .unwrap_or_else(|| panic!("no c→a path\n{}", canvas.text()));
        let max_box_bottom = canvas
            .node_boxes
            .iter()
            .map(|rect| rect.y + rect.h)
            .max()
            .expect("boxes");
        assert!(
            path.iter().any(|(_, y)| *y >= max_box_bottom),
            "back edge never reaches the bottom corridor\n{}",
            canvas.text()
        );
        for rect in &canvas.node_boxes {
            for cell in path {
                assert!(
                    cell.0 < rect.x
                        || cell.0 >= rect.x + rect.w
                        || cell.1 < rect.y
                        || cell.1 >= rect.y + rect.h,
                    "back edge enters {rect:?} at {cell:?}\n{}",
                    canvas.text()
                );
            }
        }
    }

    #[test]
    fn the_same_input_draws_the_same_map() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![hop("a", "b", false), hop("b", "c", false)];
        let world = world_size(&nodes, &edges, 2);
        let first = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
        let second = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
        assert_eq!(first.text(), second.text());
        assert_eq!(first.node_boxes, second.node_boxes);
    }

    #[test]
    fn role_box_finds_the_corner_the_map_drew() {
        let nodes = vec![node("a"), node("b")];
        let edges = Vec::new();
        let world = world_size(&nodes, &edges, 2);
        let canvas = layout(&nodes, &edges, world.0 as u16, world.1 as u16, 2);
        let drawn = canvas
            .node_boxes
            .iter()
            .find(|rect| rect.name == "b")
            .expect("the box");
        let placed = role_box(&nodes, &edges, "b", world, 2).expect("the same box");
        assert_eq!(
            (placed.x, placed.y, placed.w, placed.h),
            (drawn.x, drawn.y, drawn.w, drawn.h)
        );
    }
}
