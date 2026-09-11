use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    fn blank() -> Self {
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeBox {
    pub name: String,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![vec![Cell::blank(); width]; height],
            node_boxes: Vec::new(),
        }
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

pub fn layout(nodes: &[LayoutNode], edges: &[LayoutEdge], w: u16, h: u16) -> Canvas {
    let width = w as usize;
    let height = h as usize;
    let mut canvas = Canvas::new(width, height);
    if width < 12 || height < 4 || nodes.is_empty() {
        draw_text(&mut canvas, 0, 0, "(no roles)", CellKind::Plain);
        return canvas;
    }

    let mut sorted_nodes = nodes.to_vec();
    sorted_nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let names = sorted_nodes
        .iter()
        .map(|node| node.name.clone())
        .collect::<BTreeSet<_>>();
    let dag_edges = break_cycles(edges, &names);
    let mut layers = assign_layers(&sorted_nodes, &dag_edges);
    order_layers(&mut layers, &dag_edges);

    // Boxes sit one per layer along x, so the width budget divides by the
    // layer count. Dividing by the widest layer's row count instead let a
    // six-layer ring ask for six 28-wide boxes and space them 18 apart, which
    // drew each box over the one to its right.
    let layer_count = layers.len().max(1);
    let node_w = width_for(&sorted_nodes, width, layer_count);
    let node_h = 7usize.min(height.max(4));
    let x_step = if layer_count == 1 {
        0
    } else {
        width.saturating_sub(node_w) / (layer_count - 1).max(1)
    };
    let y_gap = 2usize;
    let mut boxes = BTreeMap::new();
    for (layer_index, layer) in layers.iter().enumerate() {
        let layer_height = layer.len() * node_h + layer.len().saturating_sub(1) * y_gap;
        let start_y = height.saturating_sub(layer_height) / 2;
        let x = if layer_count == 1 {
            width.saturating_sub(node_w) / 2
        } else {
            (layer_index * x_step).min(width.saturating_sub(node_w))
        };
        for (row_index, name) in layer.iter().enumerate() {
            let y = (start_y + row_index * (node_h + y_gap)).min(height.saturating_sub(node_h));
            boxes.insert(
                name.clone(),
                NodeBox {
                    name: name.clone(),
                    x,
                    y,
                    w: node_w,
                    h: node_h,
                },
            );
        }
    }

    let layer_of = layers
        .iter()
        .enumerate()
        .flat_map(|(index, layer)| layer.iter().map(move |name| (name.clone(), index)))
        .collect::<BTreeMap<_, _>>();
    let mut routed = dag_edges
        .iter()
        .filter(|edge| boxes.contains_key(&edge.from) && boxes.contains_key(&edge.to))
        .cloned()
        .collect::<Vec<_>>();
    routed.sort_by_cached_key(|edge| edge_key(edge, &boxes));

    // A hop that crosses a layer would run through the boxes between its ends,
    // so those hops take a channel above (or below) the band. One row per hop
    // keeps the runs parallel and the picture deterministic. Each node's hops
    // also take their own interior row, so two arrows never share a cell.
    let long_hops = routed
        .iter()
        .filter(|edge| layer_gap(edge, &layer_of) >= 2)
        .count();
    let channel = channel_rows(&boxes, long_hops, height);
    let mut exit_seen = BTreeMap::<String, usize>::new();
    let mut enter_seen = BTreeMap::<String, usize>::new();
    let mut lane = 0usize;
    for edge in &routed {
        let (Some(from), Some(to)) = (boxes.get(&edge.from), boxes.get(&edge.to)) else {
            continue;
        };
        let exit_y = {
            let index = exit_seen.entry(edge.from.clone()).or_default();
            let row = hop_row(from, *index);
            *index += 1;
            row
        };
        let enter_y = {
            let index = enter_seen.entry(edge.to.clone()).or_default();
            let row = hop_row(to, *index);
            *index += 1;
            row
        };
        let row = if layer_gap(edge, &layer_of) >= 2 {
            let row = channel.map(|base| base.for_lane(lane));
            lane += 1;
            row
        } else {
            None
        };
        route_edge(&mut canvas, from, to, edge.in_flight, row, exit_y, enter_y);
    }

    for node in &sorted_nodes {
        if let Some(rect) = boxes.get(&node.name) {
            draw_node(&mut canvas, node, rect);
            canvas.node_boxes.push(rect.clone());
        }
    }
    canvas.node_boxes.sort_by(|a, b| a.name.cmp(&b.name));
    canvas
}

fn width_for(nodes: &[LayoutNode], width: usize, columns: usize) -> usize {
    let longest = nodes
        .iter()
        .flat_map(|node| {
            let mut rows = vec![
                node.title.len() + usize::from(node.aggregate.is_some()) + usize::from(node.busy),
            ];
            rows.extend(
                node.sessions
                    .iter()
                    .take(4)
                    .map(|s| s.task.len() + s.age.len() + 4),
            );
            rows
        })
        .max()
        .unwrap_or(8);
    let by_text = (longest + 4).clamp(14, 28);
    let by_space = if columns <= 1 {
        width.clamp(14, 28)
    } else {
        ((width.saturating_sub((columns - 1) * 3)) / columns).clamp(14, 28)
    };
    by_text.min(by_space).min(width.max(1))
}

fn break_cycles(edges: &[LayoutEdge], names: &BTreeSet<String>) -> Vec<LayoutEdge> {
    let mut ordered = edges
        .iter()
        .filter(|edge| {
            edge.from != edge.to && names.contains(&edge.from) && names.contains(&edge.to)
        })
        .cloned()
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    let mut kept = Vec::new();
    let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in ordered {
        if reaches(&adjacency, &edge.to, &edge.from) {
            continue;
        }
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .insert(edge.to.clone());
        kept.push(edge);
    }
    kept
}

fn reaches(adjacency: &BTreeMap<String, BTreeSet<String>>, start: &str, target: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([start.to_string()]);
    while let Some(name) = queue.pop_front() {
        if name == target {
            return true;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(next) = adjacency.get(&name) {
            for child in next {
                queue.push_back(child.clone());
            }
        }
    }
    false
}

fn assign_layers(nodes: &[LayoutNode], edges: &[LayoutEdge]) -> Vec<Vec<String>> {
    let mut layer_by_name = nodes
        .iter()
        .map(|node| (node.name.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut incoming = nodes
        .iter()
        .map(|node| (node.name.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in edges {
        *incoming.entry(edge.to.clone()).or_default() += 1;
        outgoing
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    for targets in outgoing.values_mut() {
        targets.sort();
    }
    let mut queue = incoming
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect::<VecDeque<_>>();
    let mut visited = BTreeSet::new();
    while let Some(name) = queue.pop_front() {
        visited.insert(name.clone());
        let source_layer = layer_by_name.get(&name).copied().unwrap_or(0);
        if let Some(children) = outgoing.get(&name) {
            for child in children {
                let target_layer = layer_by_name.entry(child.clone()).or_default();
                *target_layer = (*target_layer).max(source_layer + 1);
                if let Some(count) = incoming.get_mut(child) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }
    for node in nodes {
        if !visited.contains(&node.name) {
            layer_by_name.entry(node.name.clone()).or_insert(0);
        }
    }
    let max_layer = layer_by_name.values().copied().max().unwrap_or(0);
    let mut layers = vec![Vec::new(); max_layer + 1];
    for (name, layer) in layer_by_name {
        layers[layer].push(name);
    }
    for layer in &mut layers {
        layer.sort();
    }
    layers
}

fn order_layers(layers: &mut [Vec<String>], edges: &[LayoutEdge]) {
    let mut incoming: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in edges {
        incoming
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    for source in incoming.values_mut() {
        source.sort();
    }
    for idx in 1..layers.len() {
        let prev_pos = layers[idx - 1]
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i as u32))
            .collect::<BTreeMap<_, _>>();
        layers[idx].sort_by(|a, b| {
            let ac = centroid(a, &incoming, &prev_pos);
            let bc = centroid(b, &incoming, &prev_pos);
            ac.cmp(&bc).then_with(|| a.cmp(b))
        });
    }
}

fn centroid(
    name: &str,
    incoming: &BTreeMap<String, Vec<String>>,
    prev_pos: &BTreeMap<String, u32>,
) -> u32 {
    let Some(sources) = incoming.get(name) else {
        return u32::MAX;
    };
    let mut sum = 0;
    let mut count = 0;
    for source in sources {
        if let Some(pos) = prev_pos.get(source) {
            sum += *pos;
            count += 1;
        }
    }
    sum.checked_div(count).unwrap_or(u32::MAX)
}

fn draw_node(canvas: &mut Canvas, node: &LayoutNode, rect: &NodeBox) {
    let border = match node.presence {
        Presence::Online => CellKind::Online,
        Presence::Offline => CellKind::Offline,
        Presence::Draining => CellKind::Draining,
    };
    if rect.w < 2 || rect.h < 2 {
        return;
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
    draw_text(
        canvas,
        rect.y,
        rect.x + 2,
        &title,
        if node.aggregate.is_some() {
            CellKind::Aggregate
        } else {
            CellKind::Plain
        },
    );
    if node.busy {
        let star_x = rect.x + 2 + char_len(&title);
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
            rect.x + 2,
            &truncate(&text, rect.w.saturating_sub(4)),
            CellKind::Plain,
        );
    }
    if node.sessions.len() > 4 && rect.h > 6 {
        let text = format!("+{}", node.sessions.len() - 4);
        draw_text(canvas, rect.y + 5, rect.x + 2, &text, CellKind::Plain);
    }
}

/// The rows reserved for hops that cross a layer.
///
/// Lanes grow away from the box band, so `first` is the row nearest it and
/// `step` walks outward.
#[derive(Clone, Copy, Debug)]
struct ChannelRows {
    first: usize,
    step: isize,
}

impl ChannelRows {
    fn for_lane(self, lane: usize) -> usize {
        (self.first as isize + self.step * lane as isize).max(0) as usize
    }
}

fn channel_rows(
    boxes: &BTreeMap<String, NodeBox>,
    lanes: usize,
    height: usize,
) -> Option<ChannelRows> {
    if lanes == 0 {
        return None;
    }
    let top = boxes.values().map(|rect| rect.y).min()?;
    let bottom = boxes.values().map(|rect| rect.y + rect.h).max()?;
    if top >= lanes {
        Some(ChannelRows {
            first: top - 1,
            step: -1,
        })
    } else if height.saturating_sub(bottom) >= lanes {
        Some(ChannelRows {
            first: bottom,
            step: 1,
        })
    } else {
        None
    }
}

/// One interior row of a box, so a node's hops never share an exit or entry
/// cell; a node with more hops than rows wraps, which keeps the pass total.
fn hop_row(rect: &NodeBox, index: usize) -> usize {
    let rows = rect.h.saturating_sub(2).max(1);
    rect.y + 1 + index % rows
}

fn edge_key(
    edge: &LayoutEdge,
    boxes: &BTreeMap<String, NodeBox>,
) -> (usize, usize, usize, usize, String, String) {
    let from = boxes.get(&edge.from);
    let to = boxes.get(&edge.to);
    (
        from.map(|rect| rect.x).unwrap_or(0),
        from.map(|rect| rect.y).unwrap_or(0),
        to.map(|rect| rect.x).unwrap_or(0),
        to.map(|rect| rect.y).unwrap_or(0),
        edge.from.clone(),
        edge.to.clone(),
    )
}

fn layer_gap(edge: &LayoutEdge, layer_of: &BTreeMap<String, usize>) -> usize {
    let from = layer_of.get(&edge.from).copied().unwrap_or(0);
    let to = layer_of.get(&edge.to).copied().unwrap_or(0);
    to.saturating_sub(from)
}

/// The corner of a cell the path enters from the left and leaves vertically
/// upward; the other three shapes follow from the same two directions.
fn elbow(from_left: bool, up: bool) -> char {
    match (from_left, up) {
        (true, true) => '╝',
        (true, false) => '╗',
        (false, true) => '╚',
        (false, false) => '╔',
    }
}

fn route_edge(
    canvas: &mut Canvas,
    from: &NodeBox,
    to: &NodeBox,
    active: bool,
    channel: Option<usize>,
    fy: usize,
    ty: usize,
) {
    let kind = if active {
        CellKind::ActiveEdge
    } else {
        CellKind::Edge
    };
    let x1 = from.x + from.w;
    let x2 = to.x.saturating_sub(1);
    if let Some(row) = channel {
        if x1 <= x2 && row < canvas.height {
            put(canvas, x1, fy, elbow(true, row < fy), kind);
            vline(
                canvas,
                x1,
                fy.min(row) + 1,
                fy.max(row).saturating_sub(1),
                kind,
            );
            put(canvas, x1, row, elbow(false, fy < row), kind);
            hline(canvas, x1, x2, row, kind);
            put(canvas, x2, row, elbow(true, ty < row), kind);
            vline(
                canvas,
                x2,
                row.min(ty) + 1,
                row.max(ty).saturating_sub(1),
                kind,
            );
            put(canvas, x2, ty, '▶', kind);
            return;
        }
    }
    let fx = from.x + from.w.saturating_sub(1);
    let tx = to.x;
    if tx > fx + 1 {
        let mid_x = (fx + tx) / 2;
        hline(canvas, fx + 1, mid_x, fy, kind);
        if fy != ty {
            put(canvas, mid_x, fy, if ty > fy { '╗' } else { '╝' }, kind);
            vline(
                canvas,
                mid_x,
                fy.min(ty) + 1,
                fy.max(ty).saturating_sub(1),
                kind,
            );
            put(canvas, mid_x, ty, if ty > fy { '╚' } else { '╔' }, kind);
        }
        if mid_x + 1 < tx {
            hline(canvas, mid_x + 1, tx.saturating_sub(1), ty, kind);
        }
        put(canvas, tx.saturating_sub(1), ty, '▶', kind);
    } else {
        let right = from.x.max(to.x) + from.w.min(canvas.width.saturating_sub(1));
        let detour = right.min(canvas.width.saturating_sub(2));
        hline(canvas, fx + 1, detour, fy, kind);
        if fy != ty {
            put(canvas, detour, fy, if ty > fy { '╗' } else { '╝' }, kind);
            vline(
                canvas,
                detour,
                fy.min(ty) + 1,
                fy.max(ty).saturating_sub(1),
                kind,
            );
            put(canvas, detour, ty, if ty > fy { '╚' } else { '╔' }, kind);
        }
        if tx > 0 && tx.saturating_sub(1) <= detour {
            hline(
                canvas,
                tx.saturating_sub(1),
                detour.saturating_sub(1),
                ty,
                kind,
            );
            put(canvas, tx.saturating_sub(1), ty, '▶', kind);
        }
    }
}

fn hline(canvas: &mut Canvas, x1: usize, x2: usize, y: usize, kind: CellKind) {
    if y >= canvas.height || x1 > x2 {
        return;
    }
    for x in x1..=x2.min(canvas.width.saturating_sub(1)) {
        let ch = match canvas.cells[y][x].ch {
            '║' => '╣',
            '╠' | '╣' => '╬',
            '╔' | '╚' | '╗' | '╝' | '▶' => canvas.cells[y][x].ch,
            _ => '═',
        };
        put(canvas, x, y, ch, kind);
    }
}

fn vline(canvas: &mut Canvas, x: usize, y1: usize, y2: usize, kind: CellKind) {
    if x >= canvas.width || y1 > y2 {
        return;
    }
    for y in y1..=y2.min(canvas.height.saturating_sub(1)) {
        let ch = match canvas.cells[y][x].ch {
            '═' => '╬',
            '╔' | '╚' | '╗' | '╝' | '▶' => canvas.cells[y][x].ch,
            _ => '║',
        };
        put(canvas, x, y, ch, kind);
    }
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

fn truncate(value: &str, max: usize) -> String {
    if char_len(value) <= max {
        return value.to_string();
    }
    if max == 0 {
        String::new()
    } else if max == 1 {
        "…".to_string()
    } else {
        format!("{}…", value.chars().take(max - 1).collect::<String>())
    }
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> LayoutNode {
        LayoutNode {
            name: name.to_string(),
            title: name.to_string(),
            presence: Presence::Online,
            sessions: Vec::new(),
            aggregate: None,
            busy: false,
        }
    }

    #[test]
    fn breaks_cycles_and_layers_left_to_right() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![
            LayoutEdge {
                from: "a".into(),
                to: "b".into(),
                in_flight: false,
            },
            LayoutEdge {
                from: "b".into(),
                to: "c".into(),
                in_flight: false,
            },
            LayoutEdge {
                from: "c".into(),
                to: "a".into(),
                in_flight: false,
            },
        ];
        let canvas = layout(&nodes, &edges, 80, 20);
        let a = canvas.node_boxes.iter().find(|b| b.name == "a").unwrap();
        let b = canvas.node_boxes.iter().find(|b| b.name == "b").unwrap();
        let c = canvas.node_boxes.iter().find(|b| b.name == "c").unwrap();
        assert!(a.x < b.x && b.x < c.x, "{a:?} {b:?} {c:?}");
        assert_eq!(canvas.text().matches('▶').count(), 2);
    }

    #[test]
    fn deterministic_order_and_barycenter() {
        let nodes = vec![node("a"), node("b"), node("c"), node("d")];
        let edges = vec![
            LayoutEdge {
                from: "a".into(),
                to: "d".into(),
                in_flight: false,
            },
            LayoutEdge {
                from: "b".into(),
                to: "c".into(),
                in_flight: false,
            },
        ];
        let first = layout(&nodes, &edges, 90, 24).text();
        let second = layout(&nodes, &edges, 90, 24).text();
        assert_eq!(first, second);
        let c = first.find("╭─c").unwrap();
        let d = first.find("╭─d").unwrap();
        assert!(
            d < c,
            "d follows source a and sorts before c by centroid\n{first}"
        );
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
        assert!(text.contains("abcdefg"));
        assert!(text.contains('◐'));
    }

    #[test]
    fn active_edges_are_marked_on_cells() {
        let nodes = vec![node("a"), node("b")];
        let edges = vec![LayoutEdge {
            from: "a".into(),
            to: "b".into(),
            in_flight: true,
        }];
        let canvas = layout(&nodes, &edges, 60, 10);
        assert!(
            canvas
                .cells
                .iter()
                .flatten()
                .any(|cell| cell.kind == CellKind::ActiveEdge && cell.ch == '═')
        );
        assert!(canvas.text().contains('▶'));
    }

    #[test]
    fn a_hop_across_a_layer_takes_the_channel_above_the_band() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![
            LayoutEdge {
                from: "a".into(),
                to: "b".into(),
                in_flight: false,
            },
            LayoutEdge {
                from: "b".into(),
                to: "c".into(),
                in_flight: false,
            },
            LayoutEdge {
                from: "a".into(),
                to: "c".into(),
                in_flight: true,
            },
        ];
        let canvas = layout(&nodes, &edges, 96, 24);
        let b = canvas
            .node_boxes
            .iter()
            .find(|rect| rect.name == "b")
            .expect("the middle box");
        for y in b.y + 1..b.y + b.h - 1 {
            for x in b.x + 1..b.x + b.w - 1 {
                assert_eq!(
                    canvas.cells[y][x].ch,
                    ' ',
                    "the crossing hop leaves the middle box clear\n{}",
                    canvas.text()
                );
            }
        }
        let top = canvas.node_boxes.iter().map(|rect| rect.y).min().unwrap();
        assert!(
            canvas.cells[..top]
                .iter()
                .flatten()
                .any(|cell| cell.kind == CellKind::ActiveEdge && cell.ch == '═'),
            "the crossing hop runs above the band and keeps its highlight\n{}",
            canvas.text()
        );
        let text = canvas.text();
        assert_eq!(text.matches('▶').count(), 3, "{text}");
    }
}
