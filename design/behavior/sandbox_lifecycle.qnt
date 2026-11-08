// REQ-6, REQ-7
// 沙箱租约生命周期与安全网巡检行为模型
module sandbox_lifecycle {
  import contract.* from "../.gen/contract"
  import onlyne_computeBroker as B from "../.gen/contract"

  var lastCall: Call
  var ledger: B::Ledger
  var leases: Set[str]

  action init = all {
    lastCall' = NoCall,
    ledger' = B::initial,
    leases' = Set(),
  }

  // 1. 申领算力 (acquire)
  action doAcquire(id: str, ttl: int): bool = all {
    not(id.in(leases)),
    ttl > 0,
    lastCall' = onlyne_computeBroker(B::acquire({ lid: id, ttl: ttl })),
    ledger' = { ...ledger, running: ledger.running.put(id, ttl) },
    leases' = leases.union(Set(id)),
  }

  // 2. 续约 (renew)
  action doRenew(id: str, add_ms: int): bool = all {
    id.in(ledger.running.keys()),
    add_ms > 0,
    all {
      val cur_ttl = ledger.running.get(id)
      all {
        lastCall' = onlyne_computeBroker(B::renew({ lid: id, add_ms: add_ms })),
        ledger' = { ...ledger, running: ledger.running.put(id, cur_ttl + add_ms) },
      }
    },
    leases' = leases,
  }

  // 3. 主动释放/停机 (release)
  action doRelease(id: str): bool = all {
    id.in(ledger.running.keys()),
    lastCall' = onlyne_computeBroker(B::release({ lid: id })),
    ledger' = {
      ...ledger,
      running: ledger.running.keys().exclude(Set(id)).mapBy(k => ledger.running.get(k)),
      stopped: ledger.stopped.union(Set(id))
    },
    leases' = leases,
  }

  // 4. 彻底销毁 (destroy)
  action doDestroy(id: str): bool = all {
    id.in(ledger.running.keys()) or id.in(ledger.stopped),
    lastCall' = onlyne_computeBroker(B::destroy({ lid: id, result: B::Closed_Closed })),
    ledger' = {
      ...ledger,
      running: ledger.running.keys().exclude(Set(id)).mapBy(k => ledger.running.get(k)),
      stopped: ledger.stopped.exclude(Set(id)),
      destroyed: ledger.destroyed.union(Set(id))
    },
    leases' = leases,
  }

  // 安全性质：互斥性 —— 运行态、停机态和销毁态三个集合彼此不相交
  val safety =
    ledger.running.keys().intersect(ledger.stopped).size() == 0 and
    ledger.running.keys().intersect(ledger.destroyed).size() == 0 and
    ledger.stopped.intersect(ledger.destroyed).size() == 0

  action step = {
    nondet id = Set("s1", "s2").oneOf()
    any {
      doAcquire(id, 100),
      doRenew(id, 50),
      doRelease(id),
      doDestroy(id)
    }
  }
}
