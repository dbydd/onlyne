import GrugMatic.Physics

namespace GrugMatic

/-- A ledger is the authoritative truth assignment, outside every model context. -/
structure Ledger where
  committed : Fact → Bool

/-- An external run keeps ambient rot separate from ledger, fresh views, and transport accounting. -/
structure ExternalRun where
  ledger : Round → Ledger
  ambient : Round → Ctx
  view : Round → Ctx
  proseSource : String
  instruction : Round → String
  tasksInSession : Round → Nat
  transportSteps : Round → Nat
  epsilon : Nat
  violation : Round → Nat

/-- Every round recomputes its context view from the authoritative ledger at that round. -/
def FreshView (run : ExternalRun) (render : Ledger → Ctx) : Prop :=
  ∀ n, run.view n = render (run.ledger n)

/-- Every round refetches its instruction from the single prose source. -/
def FreshProse (run : ExternalRun) : Prop :=
  ∀ n, run.instruction n = run.proseSource

/-- A run is one-shot when a session sees at most one task. -/
def OneShot (run : ExternalRun) : Prop :=
  ∀ n, run.tasksInSession n ≤ 1

/-- A run is transport-bounded when context depth contributes no term to risk. -/
def TransportBounded (run : ExternalRun) : Prop :=
  ∀ n, run.violation n ≤ run.transportSteps n * run.epsilon

/-- `ledgerModel` constructs the safe run; the ambient trace is deliberately not consulted. -/
def ledgerModel (ledger : Round → Ledger) (render : Ledger → Ctx) (prose : String)
    (ambient : Round → Ctx) (active : Round → Bool)
    (steps : Round → Nat) (epsilon : Nat) : ExternalRun where
  ledger := ledger
  ambient := ambient
  view := fun n => render (ledger n)
  proseSource := prose
  instruction := fun _ => prose
  tasksInSession := fun n => if active n then 1 else 0
  transportSteps := steps
  epsilon := epsilon
  violation := fun n => steps n * epsilon

/-- The external ledger model has fresh views and prose, one-shot sessions, and a risk bound unchanged by arbitrary context rot. -/
theorem grug_ledger_rescue (ledger : Round → Ledger) (render : Ledger → Ctx)
    (prose : String) (ambient : Round → Ctx) (active : Round → Bool)
    (steps : Round → Nat) (epsilon : Nat) :
    let run := ledgerModel ledger render prose ambient active steps epsilon
    FreshView run render ∧
      FreshProse run ∧
      OneShot run ∧
      TransportBounded run ∧
      ∀ (otherAmbient : Round → Ctx) n,
        run.violation n =
          (ledgerModel ledger render prose otherAmbient active steps epsilon).violation n := by
  simp only [FreshView, FreshProse, OneShot, TransportBounded, ledgerModel]
  refine ⟨fun _ => True.intro, fun _ => True.intro, ?_,
    fun _ => Nat.le_refl _, fun _ _ => True.intro⟩
  intro n
  cases hactive : active n <;> simp

/-- Replacing the entire context trace cannot alter the ledger's authoritative truth. -/
theorem ledger_authority_ignores_context (ledger : Round → Ledger)
    (render : Ledger → Ctx) (prose : String) (left right : Round → Ctx)
    (active : Round → Bool) (steps : Round → Nat) (epsilon : Nat)
    (n : Round) (fact : Fact) :
    ((ledgerModel ledger render prose left active steps epsilon).ledger n).committed fact =
      ((ledgerModel ledger render prose right active steps epsilon).ledger n).committed fact := by
  rfl

end GrugMatic
