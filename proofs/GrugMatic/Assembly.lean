import GrugMatic.Decisions

namespace GrugMatic

/-- A thin design packages the eight decisions that remove persistent context obligations. -/
structure ThinDesign where
  carrier : Carrier
  carrierValid : carrier.valid
  ledger : Round → Ledger
  proseSource : String
  active : Round → Bool
  transportSteps : Round → Nat
  epsilon : Nat
  fileTruth : String
  supervisorMemory : Option String
  task : Fact
  taskIsFresh : task ≠ .priorTask

/-- Onlyne's assembly carries each decision witness and the resulting external run. -/
structure SafeAssembly where
  run : ExternalRun
  carrierBound : Nat
  carrierBoundProof : carrierBound ≤ 2
  noMarkdownMemory : Fact.markdownSemantics ∉ run.view 0
  splitAuthority : authorityOf .routingLedger ≠ authorityOf .executionState
  noRoutingMemory : Fact.routingLedger ∉ run.view 0
  noExecutionMemory : Fact.executionState ∉ run.view 0
  noWorkspaceMemory : Fact.workspaceContent ∉ run.view 0
  noDeliveryMemory : Fact.deliveryMemory ∉ run.view 0
  noScheduleMemory : Fact.schedulingTable ∉ run.view 0
  noConfigMemory : Fact.runtimeConfig ∉ run.view 0
  noSupervisorMemory : Fact.supervisorIdentity ∉ run.view 0
  noProseMemory : Fact.rememberedProse ∉ run.view 0
  noPriorTaskMemory : Fact.priorTask ∉ run.view 0
  freshView : FreshView run (fun _ => run.view 0)
  freshProse : FreshProse run
  oneShot : OneShot run

/-- The thin rendering keeps only the current task after carrier validation happens at the boundary. -/
def thinView (_ledger : Ledger) : Ctx := [.currentTask]

/-- The assembled Onlyne design exhibits a transport-only risk bound and none of the removed facts in persistent context. -/
theorem grug_thin_and_local (design : ThinDesign) :
    ∃ assembly : SafeAssembly, TransportBounded assembly.run := by
  -- Ledger ↔ onlyne-store; fresh prose ↔ welcome/prose_cache; transport ↔ frame/net.
  have hd4 := d4_carrier_minimality design.carrier design.carrierValid
  have hd5 := d5_authority_split
  have hd6 := d6_content_by_reference (fun _ => []) (fun _ => []) ⟨"artifact://content"⟩
  have hd11 := d11_idempotent_redelivery [⟨0, "payload"⟩] ⟨0, "payload"⟩ (by simp) 2
  have hd12 := d12_delivery_creates_task 0 "task"
  have hd13 := d13_file_truth_reload design.fileTruth none design.supervisorMemory
  have hd15 := d15_wild_supervisor_single_prose
    design.proseSource none design.supervisorMemory
  have hone := one_shot_session_bound design.task design.taskIsFresh
  let run := ledgerModel design.ledger thinView design.proseSource
    (fun _ => []) design.active design.transportSteps design.epsilon
  have hrescue := grug_ledger_rescue design.ledger thinView design.proseSource
    (fun _ => []) design.active design.transportSteps design.epsilon
  refine ⟨{
    run := run
    carrierBound := design.carrier.contextFacts.length
    carrierBoundProof := hd4.1
    noMarkdownMemory := by simp [run, ledgerModel, thinView]
    splitAuthority := hd5.2.2.1
    noRoutingMemory := by simp [run, ledgerModel, thinView]
    noExecutionMemory := by simp [run, ledgerModel, thinView]
    noWorkspaceMemory := by simp [run, ledgerModel, thinView]
    noDeliveryMemory := by simp [run, ledgerModel, thinView]
    noScheduleMemory := by simp [run, ledgerModel, thinView]
    noConfigMemory := by simp [run, ledgerModel, thinView]
    noSupervisorMemory := by simp [run, ledgerModel, thinView]
    noProseMemory := by simp [run, ledgerModel, thinView]
    noPriorTaskMemory := by simp [run, ledgerModel, thinView]
    freshView := hrescue.1
    freshProse := hrescue.2.1
    oneShot := hrescue.2.2.1
  }, hrescue.2.2.2.1⟩

end GrugMatic
