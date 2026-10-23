# Third-party notices

## graphtatui — removed

v1's TUI ported a force-directed map into `crates/onlyne-cli/src/tui` from
[graphtatui](https://github.com/sok205/graphtatui), (c) Sok205 contributors,
dual-licensed under the MIT and Apache-2.0 licences.

That map is deleted: v2's TUI is the three-page board of `docs/v2-PLAN.md`
§"TUI" (lines 393-401), where a role's place is a row in a table and no layout
computes one. The derived files — `force.rs`, `layout.rs`, and the `explorer/`
tree under `crates/onlyne-cli/src/tui/` — are gone with it, so no graphtatui
material is distributed in this repository and no upstream licence text is
reproduced here.
