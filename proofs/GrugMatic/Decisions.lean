import GrugMatic.External

namespace GrugMatic

/-- A role marks which side is authoritative for a class of state. -/
inductive Authority where
  | server
  | client
  deriving DecidableEq, BEq, Repr

/-- A content reference names external bytes without placing those bytes in context. -/
structure ContentRef where
  uri : String
  deriving DecidableEq, BEq, Repr

/-- An operation carries a stable key and payload so retries can consult durable receipts. -/
structure Operation where
  opId : Nat
  payload : String
  deriving DecidableEq, BEq, Repr

/-- A durable receipt records the payload accepted for one operation key. -/
structure Receipt where
  opId : Nat
  payload : String
  deriving DecidableEq, BEq, Repr

/-- A task is created directly from delivery, without a central scheduling-memory row. -/
structure Task where
  taskId : Nat
  body : String
  deriving DecidableEq, BEq, Repr

/-- Carrier material is exactly optional text and optional one-image bytes. -/
structure Carrier where
  text : Option String
  imageBytes : Option (List UInt8)
  deriving Repr

/-- The text budget is the protocol's one-megabyte UTF-8 limit. -/
def maxTextBytes : Nat := 1024 * 1024

/-- The image budget is the protocol's two-megabyte decoded-byte limit. -/
def maxImageBytes : Nat := 2 * 1024 * 1024

/-- The carrier facts visible to the coordination model exclude every removed media concern. -/
def Carrier.contextFacts (carrier : Carrier) : Ctx :=
  (if carrier.text.isSome then [.currentTask] else []) ++
    (if carrier.imageBytes.isSome then [.richAttachment] else [])

/-- The protocol validates UTF-8 text and decoded image bytes at the carrier boundary. -/
def Carrier.valid (carrier : Carrier) : Prop :=
  (match carrier.text with
  | none => True
  | some text => String.utf8ByteSize text ≤ maxTextBytes) ∧
    (match carrier.imageBytes with
    | none => True
    | some image => image.length ≤ maxImageBytes)

/-- D4 bounds each message to text plus at most one image with fixed byte budgets. -/
theorem d4_carrier_minimality (carrier : Carrier) (hvalid : carrier.valid) :
    (carrier.contextFacts.length ≤ 2) ∧
      Fact.markdownSemantics ∉ carrier.contextFacts ∧
      (∀ text, carrier.text = some text →
        String.utf8ByteSize text ≤ maxTextBytes) ∧
      (∀ image, carrier.imageBytes = some image → image.length ≤ maxImageBytes) := by
  constructor
  · cases htext : carrier.text <;> cases himage : carrier.imageBytes <;>
      simp [Carrier.contextFacts, htext, himage]
  constructor
  · cases htext : carrier.text <;> cases himage : carrier.imageBytes <;>
      simp [Carrier.contextFacts, htext, himage]
  constructor
  · intro text htext
    cases hcarrier : carrier.text with
    | none => simp_all
    | some old =>
      have hsame : text = old := Option.some.inj (htext.symm.trans hcarrier)
      simpa [Carrier.valid, hcarrier, hsame] using hvalid.1
  · intro image himage
    cases hcarrier : carrier.imageBytes with
    | none => simp_all
    | some old =>
      have hsame : image = old := Option.some.inj (himage.symm.trans hcarrier)
      simpa [Carrier.valid, hcarrier, hsame] using hvalid.2

/-- Authority splitting stores routing in the server and execution state in the client. -/
def authorityOf : Fact → Authority
  | .executionState => .client
  | _ => .server

/-- D5 gives ledger and execution state distinct authorities, so neither must remember the other. -/
theorem d5_authority_split :
    authorityOf .routingLedger = .server ∧
      authorityOf .executionState = .client ∧
      authorityOf .routingLedger ≠ authorityOf .executionState ∧
      Fact.routingLedger ∉ [.currentTask] ∧
      Fact.executionState ∉ [.currentTask] := by
  simp [authorityOf]

/-- Resolving a content reference fetches bytes externally while the context carries only a reference fact. -/
def fetchByReference (store : String → List UInt8) (reference : ContentRef) : List UInt8 :=
  store reference.uri

/-- D6 passes workspace content by reference, so changing fetched bytes never inserts workspace content into context. -/
theorem d6_content_by_reference (storeA storeB : String → List UInt8)
    (reference : ContentRef) :
    Fact.workspaceContent ∉ [Fact.currentTask] ∧
      fetchByReference storeA reference = storeA reference.uri ∧
      fetchByReference storeB reference = storeB reference.uri := by
  simp [fetchByReference]

/-- Receipt lookup either records a first operation or reuses the matching durable receipt. -/
def acceptOperation (receipts : List Receipt) (operation : Operation) : List Receipt × Bool :=
  match receipts.find? (fun receipt => receipt.opId == operation.opId) with
  | none => (⟨operation.opId, operation.payload⟩ :: receipts, true)
  | some receipt => (receipts, receipt.payload == operation.payload)

/-- A redelivery repeats lookup against the same durable receipt table. -/
def redeliver (receipts : List Receipt) (operation : Operation) : Nat → List Receipt × Bool
  | 0 => acceptOperation receipts operation
  | n + 1 => redeliver receipts operation n

/-- D11 any number of redeliveries reuses one receipt without requiring delivery memory in context. -/
theorem d11_idempotent_redelivery (receipts : List Receipt) (operation : Operation)
    (hfind : receipts.find? (fun receipt => receipt.opId == operation.opId) =
      some ⟨operation.opId, operation.payload⟩) :
    ∀ attempts, redeliver receipts operation attempts = (receipts, true) ∧
      Fact.deliveryMemory ∉ [Fact.currentTask] := by
  intro attempts
  constructor
  · induction attempts with
    | zero => simp [redeliver, acceptOperation, hfind]
    | succ n ih => simpa [redeliver] using ih
  · simp

/-- Direct delivery constructs its task and carries no independent scheduling table. -/
def taskFromDelivery (taskId : Nat) (body : String) : Task × Ctx :=
  (⟨taskId, body⟩, [.currentTask])

/-- D12 delivery creates the task itself, removing a scheduling table from the carried facts. -/
theorem d12_delivery_creates_task (taskId : Nat) (body : String) :
    (taskFromDelivery taskId body).1 = ⟨taskId, body⟩ ∧
      Fact.schedulingTable ∉ (taskFromDelivery taskId body).2 := by
  simp [taskFromDelivery]

/-- Reload replaces the active spec from the file, independent of anything remembered in context. -/
def reloadSpec (fileTruth : String) (_remembered : Option String) : String :=
  fileTruth

/-- D13 reload makes file configuration authoritative and removes runtime config from context. -/
theorem d13_file_truth_reload (fileTruth : String) (left right : Option String) :
    reloadSpec fileTruth left = reloadSpec fileTruth right ∧
      Fact.runtimeConfig ∉ [Fact.currentTask] := by
  simp [reloadSpec]

/-- Welcome material is refetched from one source while a supervisor remains an ordinary role. -/
def supervisorWelcome (proseSource : String) (_remembered : Option String) : String × Ctx :=
  (proseSource, [.currentTask])

/-- D15 keeps supervisor identity out of lifecycle and always refetches prose from its single source. -/
theorem d15_wild_supervisor_single_prose (source : String)
    (left right : Option String) :
    (supervisorWelcome source left).1 = (supervisorWelcome source right).1 ∧
      Fact.supervisorIdentity ∉ (supervisorWelcome source left).2 ∧
      Fact.rememberedProse ∉ (supervisorWelcome source left).2 := by
  simp [supervisorWelcome]

/-- A session context contains prior-task state exactly when reuse is enabled. -/
def sessionContext (reuse : Bool) (task : Fact) : Ctx :=
  if reuse then [.priorTask, task] else [task]

/-- Disabling session reuse bounds each session to one task and removes prior-task persistence. -/
theorem one_shot_session_bound (task : Fact) (hnew : task ≠ Fact.priorTask) :
    (sessionContext false task).length ≤ 1 ∧
      Fact.priorTask ∉ sessionContext false task := by
  simp [sessionContext, Ne.symm hnew]

end GrugMatic
