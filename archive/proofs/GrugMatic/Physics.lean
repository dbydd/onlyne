import Std

namespace GrugMatic

/-- Facts are the pieces of coordination state that a prompt might be forced to remember. -/
inductive Fact where
  | invariant
  | richAttachment
  | markdownSemantics
  | routingLedger
  | executionState
  | workspaceContent
  | deliveryMemory
  | schedulingTable
  | runtimeConfig
  | supervisorIdentity
  | rememberedProse
  | priorTask
  | currentTask
  deriving DecidableEq, BEq, Repr

/-- A context is the finite list of facts still carried by an LLM session. -/
abbrev Ctx := List Fact

/-- Rounds count handoffs or protocol hops. -/
abbrev Round := Nat

/-- The reliability scale is an elementary replacement for real probabilities. -/
def reliabilityScale : Nat := 1000

/-- A trace deepens when every later round has strictly more context to carry. -/
def Deepening (trace : Round → Ctx) : Prop :=
  ∀ n, (trace n).length < (trace (n + 1)).length

/-- A fact is context-carried when every round must retain it. -/
def Carried (trace : Round → Ctx) (fact : Fact) : Prop :=
  ∀ n, fact ∈ trace n

namespace ContextRot

/-- `rely ctx fact` is the retained reliability score for a fact in a context. -/
axiom rely : Ctx → Fact → Nat

/-- Growing a context cannot improve the reliability of a fact it must retain. -/
axiom monotone_decay (fact : Fact) {small large : Ctx}
    (hsize : small.length ≤ large.length) :
    rely large fact ≤ rely small fact

/-- Any fact carried through an ever-deepening context eventually has zero reliability. -/
axiom eventual_collapse (trace : Round → Ctx) (fact : Fact)
    (hdeep : Deepening trace) (hcarried : Carried trace fact) (after : Round) :
    ∃ n, after ≤ n ∧ rely (trace n) fact = 0

end ContextRot

/-- A protocol exposes its context trace, coordination invariant, and settlement claim. -/
structure Protocol where
  context : Round → Ctx
  invariant : Fact
  settles : Round → Prop

/-- An in-context protocol carries its invariant as context grows and can settle only by recalling it. -/
structure InContext (protocol : Protocol) : Prop where
  deepens : Deepening protocol.context
  carries : Carried protocol.context protocol.invariant
  context_decides : ∀ n,
    protocol.settles n ↔ 0 < ContextRot.rely (protocol.context n) protocol.invariant

/-- Violation risk is the missing reliability, capped on a 1000-unit scale. -/
noncomputable def violationRisk (protocol : Protocol) (n : Round) : Nat :=
  reliabilityScale - min reliabilityScale
    (ContextRot.rely (protocol.context n) protocol.invariant)

/-- Every in-context protocol eventually violates every nontrivial risk bound after any requested round. -/
theorem grug_impossibility (protocol : Protocol) (hctx : InContext protocol)
    (after allowed : Nat) (hallowed : allowed < reliabilityScale) :
    ∃ n, after ≤ n ∧ ¬ protocol.settles n ∧ allowed < violationRisk protocol n := by
  obtain ⟨n, hn, hzero⟩ := ContextRot.eventual_collapse
    protocol.context protocol.invariant hctx.deepens hctx.carries after
  refine ⟨n, hn, ?_, ?_⟩
  · intro hsettles
    have hpositive := (hctx.context_decides n).mp hsettles
    simp [hzero] at hpositive
  · simpa [violationRisk, hzero] using hallowed

end GrugMatic
