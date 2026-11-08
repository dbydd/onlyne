// 由 ssot gen 从 contract/*.vdmsl 生成；只读，不要手改。
module Vdm {
  type Opt[a] = | Nil | Some(a)
}
module onlyne_computeBroker {
  type LeaseId = str
  type Closed = | Closed_Closed
  type Ledger = { running: LeaseId -> int, stopped: Set[LeaseId], destroyed: Set[LeaseId] }
  pure val initial: Ledger = { running: Map(), stopped: Set(), destroyed: Set() }
  type Call =
    | acquire({ lid: LeaseId, ttl: int })
    | renew({ lid: LeaseId, add_ms: int })
    | release({ lid: LeaseId })
    | destroy({ lid: LeaseId, result: Closed })
}
module contract {
  import onlyne_computeBroker as onlyne_computeBroker
  type Call =
    | NoCall
    | onlyne_computeBroker(onlyne_computeBroker::Call)
}
