# BRIEF — grug-matic: a Lean 4 formalization of context rot (纯整活,认真证)

You are the prover, and this file is the contract. Work ONLY inside `proofs/`
(you may read the repo). Deliverable: a lake project that compiles clean with
zero `sorry`, plus a closing report. A fun register is welcome. Sloppiness is not.

## Thesis to formalize

Multi-agent systems that carry coordination state inside LLM context rot.
State kept in the context degrades with depth. State kept outside does not.
This makes thin, externalized designs (ledger, single-source prose, one-shot
sessions, carrier minimality) a theorem, not a vibe. Our repo (Onlyne v1)
exhibits the safe side.

## Ground rules (hard)

- Lean 4 core first. Plain `lean --version` is 4.33.x, and elan/lake are on PATH.
  You own your environment. If the development genuinely needs a library
  (mathlib for real probability, aesop for tactics), install it yourself:
  `elan toolchain install`, `lake update` inside `proofs/`, pin the toolchain
  in `proofs/lean-toolchain`, and justify the import in the report's editorial.
  Each dep must buy a theorem, not a vibe. The default answer stays core-only.
- Loop until: `cd proofs && lake build` exits 0 AND `grep -rn "sorry"` is empty.
  `axiom` occurrences live only in a clearly named `Physics`/`ContextRot`
  section. The postulates are the product, so enumerate them in the report.
- Every theorem gets a docstring: one sentence, plain language. Chinese is
  fine. So is grug.
- Keep the whole development under ~600 lines across all files. Elegance beats
  coverage.

## Suggested axiomatic interface (adapt freely — you are the mathematician)

- Context is a list of retained facts `Ctx`; rounds/hops are `n : Nat`.
- A reliability function `rely : Ctx → Fact → ℚ` (or a Nat-bounded error budget —
  your choice; keep it elementary), with postulates in section `ContextRot`:
  monotone decay in context size, and eventual collapse — as rounds grow, the
  reliability of any context-carried invariant tends below every positive bound.
- A coordination invariant `Settles`, defined over a protocol run. A protocol is
  `InContext` when invariant truth at round n is a function of the context
  alone.
- Theorem `grug_impossibility` (name it freely): every `InContext` protocol has
  a round past which no bound on violation probability holds. Carried state
  cannot outlive the rot.
- Theorem `grug_ledger_rescue`: exhibit the external-store model. State lives in
  `Ledger : Type`, which `rely` never touches. Context is a recomputable view: a
  fresh view per round equals one-shot sessions, and prose refetched from one
  source equals `prose_cache` + `welcome`. The violation bound depends only on
  transport steps (an `n * ε` style bound), independent of rot depth.
- Combinator lemmas, one per design decision. Each states that the combinator
  removes fact X from context persistence across hops, which lowers in-context
  load. Cover at least:
  D4 carrier minimality (text + ≤1 image: bounded per-hop bytes),
  D5 server ledger vs client execution state (authority split),
  D6 no workspace sync (content by reference: file never enters context),
  D11 at-least-once + `op_id` idempotence (redelivery without memory),
  D12 delivery creates tasks (no scheduling table to remember),
  D13 spec as file truth + reload (config never carried in context),
  D15 wild supervisor + prose single-source (operator identity outside
  lifecycle; instruction refetched, never memorized),
  one-shot sessions (reuse = false ⇒ per-session context depth bounded by 1 task).
- Closing theorem `grug_thin_and_local`: assemble the lemmas, and the Onlyne
  design is one concrete protocol with the rot-independent bound of
  `grug_ledger_rescue`. A comment line may map each constant to its file/crate
  here (e.g. Ledger ↔ crates/onlyne-store).

## Report format (your final message)

1. `lake build` outcome verbatim tail.
2. theorem inventory: name + one-line statement.
3. axiom list from `ContextRot` (postulates are the honest part — enumerate).
4. sorry count (must be 0) and grep command output proving it.
5. three lines max of editorial: where the formalization bites, where it
  flatters the design.
