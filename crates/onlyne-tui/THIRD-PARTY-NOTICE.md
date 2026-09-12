# Third-party notices

## graphtatui — force layout, radial seeding, and semantic zoom

Parts of `crates/onlyne-tui` are derived from
[graphtatui](https://github.com/sok205/graphtatui), (c) Sok205 contributors,
dual-licensed under the MIT and Apache-2.0 licences.

Derived material, all in the page-1 role map:

| Upstream | Ported as | What came over |
| --- | --- | --- |
| `src/tui/explorer/layout.rs` | `crates/onlyne-tui/src/force.rs` | The Fruchterman–Reingold iteration (`step`), its parameters, the per-node displacement clamp, and the temperature/cooling schedule |
| `src/tui/explorer/radial.rs` | `crates/onlyne-tui/src/force.rs` | Concentric-ring seeding by BFS hop distance, parent-angle ring ordering, the per-ring angle stagger, and the golden-angle seed spiral for newcomers |
| `src/tui/explorer/lod.rs` | `crates/onlyne-tui/src/force.rs` | `bfs_distances`, the zoom-to-detail-radius tiers, `NodeLod` classification, tier labels |
| `src/tui/explorer/mod.rs` | `crates/onlyne-tui/src/force.rs`, `crates/onlyne-tui/src/layout.rs` | Focus pinning while relaxing, reheat-and-settle on a topology change, the camera's zoom/pan model and its pan clamp |

Every derived file carries the header comment
`// Derived in part from graphtatui (c) Sok205 contributors, MIT/Apache-2.0.`

Deliberate departures from upstream:

* Repulsion is evaluated over every pair instead of through a Barnes–Hut
  quadtree. Upstream's quadtree only approximates at `theta > 0`; a role graph is
  tens of nodes, so the exact pass is the same force field with a fixed,
  order-independent summation.
* Rendering is written here, not ported. Upstream draws circles and Braille
  strokes, including diagonal ones; this TUI draws rounded label boxes joined by
  horizontal and vertical runs only (Manhattan routing, at most two corners),
  with `▸ ◂ ▴ ▾` arrowheads taken from the direction of the last run.
* An anti-overlap sweep (`force::separate`) runs after relaxation, because the
  boxes here have real height and width in cells.
* Ring seeding takes a gap per axis, so rings come out elliptical in layout
  units and round on screen: a terminal cell is about twice as tall as it is
  wide (`layout::Y_CELL_SCALE` packs eight tenths of a layout unit into a row).
* The settled map is normalised to its boxes before it is drawn
  (`layout::fit`): `n` boxes want about `sqrt(n)` of them per side, which is what
  frames a terminal pane. The repulsion knob scales that target, so a wider knob
  opens the map out.
* `0` recentres the camera on the map rather than resetting a fit-all zoom:
  zoom is a scale factor over the layout (1.0 = one layout unit per cell), and
  the pane is a window that pans over a map bigger than itself. Upstream's
  fit-all framing cannot work with fixed-size box labels.
* A box shows its title and up to two session rows, not four: the map is the
  page, and the detail pane carries the rest.

Nothing else from graphtatui was taken: no storage layer, no triple/RDF model,
no CLI, palette, fuzzy search, minimap, super-node aggregation, or themes.

Both upstream licences are reproduced by their canonical texts:

* MIT: <https://github.com/sok205/graphtatui/blob/main/LICENSE-MIT>
* Apache-2.0: <https://github.com/sok205/graphtatui/blob/main/LICENSE-APACHE>
