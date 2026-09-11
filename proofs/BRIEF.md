# BRIEF — grug-matic: a Lean 4 formalization of context rot (纯整活,认真证)

You are the prover. This file is the contract. Work ONLY inside `proofs/`
(plus reading the repo). Deliverable: a lake project that compiles clean with
zero `sorry`, plus a closing report. Fun register is welcome; sloppiness is not.

## Thesis to formalize

Multi-agent systems that carry coordination state inside LLM context rot:
state kept in the context degrades with depth; state kept outside does not.
From this, the necessity of thin, externalized designs (ledger, single-source
prose, one-shot sessions, carrier minimality) follows as a theorem, not a
vibe. Our repo (Onlyne v1) is the exhibited model of the safe side.

## Ground rules (hard)

- Lean 4 core first. Plain `lean --version` is 4.33.x and elan/lake are on PATH.
  You own your environment: if the development genuinely needs a library
  (mathlib for real probability, aesop for tactics), install it yourself —
  `elan toolchain install`, `lake update` inside `proofs/` — pin the toolchain
  in `proofs/lean-toolchain`, and justify the import in the report's editorial
  (each dep must buy a theorem, not a vibe). Default answer stays core-only.
- Loop until: `cd proofs && lake build` exits 0 AND `grep -rn "sorry"` is empty;
  `axiom` occurrences live only in a clearly named `Physics`/`ContextRot`
  section (the postulates are the product; enumerate them in the report).
- Every theorem gets a docstring, one sentence, plain language, may include
  Chinese, may include grug.
- Keep the whole development under ~600 lines across files; elegance beats
  coverage.

## Suggested axiomatic interface (adapt freely — you are the mathematician)

- Context as a list of retained facts `Ctx`; rounds/hops `n : Nat`.
- A reliability function `rely : Ctx → Fact → ℚ` (or Nat-bounded error budget —
  your choice, keep it elementary) with postulates in section `ContextRot`:
  monotone decay in context size; eventual collapse: reliability of any
  context-carried invariant tends below every positive bound as rounds grow.
- Coordination invariant `Settles` defined over a protocol run; a protocol is
  `InContext` when invariant truth at round n is a function of the context
  alone.
- Theorem `grug_impossibility` (name freely): every `InContext` protocol has a
  round past which no bound on violation probability holds — carried state
  cannot outlive the rot.
- Theorem `grug_ledger_rescue`: exhibit the external-store model — state lives
  in `Ledger : Type` never touched by `rely`; context is a recomputable view
  (fresh view per round = one-shot sessions; prose refetched from one source =
  `prose_cache` + `welcome`); violation bound depends only on transport steps
  (`n * ε` style), independent of rot depth.
- Combinator lemmas, one per design decision, each stating that the
  combinator removes fact X from context persistence across hops (thereby
  lowering in-context load). Cover at least:
  D4 carrier minimality (text + ≤1 image: bounded per-hop bytes),
  D5 server ledger vs client execution state (authority split),
  D6 no workspace sync (content by reference: file never enters context),
  D11 at-least-once + `op_id` idempotence (redelivery without memory),
  D12 delivery creates tasks (no scheduling table to remember),
  D13 spec as file truth + reload (config never carried in context),
  D15 wild supervisor + prose single-source (operator identity outside
  lifecycle; instruction refetched, never memorized),
  one-shot sessions (reuse = false ⇒ per-session context depth bounded by 1 task).
- Closing theorem `grug_thin_and_local`: assembling the lemmas, the Onlyne
  design is one concrete protocol with the rot-independent bound of
  `grug_ledger_rescue`. A comment line may map each constant to its
  file/crate here (e.g. Ledger ↔ crates/onlyne-store).

## Report format (your final message)

1. `lake build` outcome verbatim tail.
2. theorem inventory: name + one-line statement.
3. axiom list from `ContextRot` (postulates are the honest part — enumerate).
4. sorry count (must be 0) and grep command output proving it.
5. three lines max of editorial: where the formalization bites, where it
  flatters the design.
