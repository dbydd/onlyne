# 绑定连接

```vdm
module Binding
imports
  from Identity types NodeId renamed NodeId
exports
  types Revision; EventId; struct Prepare
  operations
    Receive: EventId ==> ();
    Handover: NodeId * Revision ==> ();
    Commit: () ==> ();
    Abort: () ==> ();
    Ack: EventId * NodeId ==> ();
    Expire: EventId ==> ();
    Recipient: EventId ==> NodeId
definitions
types
  -- 版本；每次提交获得从未使用的值，旧值不复用
  Revision = nat;

  -- 资源内事件编号；退役后不再分配给另一个事件
  EventId = token;

  -- 一次交接准备；提交与中止都留痕，双方按操作 ID 查回
  Prepare ::
    target : NodeId  -- 接受接手的节点
    expected : Revision;  -- 发起时看到的版本

  -- 绑定面：当前归属、版本、事件处置与未终结准备
state BindingState of
  assignee : NodeId  -- 当前有权处理新事件的节点
  revision : Revision  -- 当前绑定版本
  firstOwner : map EventId to NodeId  -- 每条事件首次接收时的归属
  acked : set of EventId  -- 接收者已持久保存的事件
  expired : set of EventId  -- 保留期已清除投递义务的事件
  prepare : [Prepare]  -- 尚未终结的交接准备
  inv mk_BindingState(-, r, f, a, x, p) ==
    (p = nil or p.expected = r) and (a union x) subset dom f
  init r == r = mk_BindingState(mk_token(1), 0, {|->}, {}, {}, nil)
end

operations
  -- 首次接收事件；归属取当时的 assignee，交接不改写它
  Receive(e: EventId)
  ext rd assignee wr firstOwner
  post if e in set dom firstOwner~
       then firstOwner = firstOwner~
       else firstOwner = firstOwner~ ++ {e |-> assignee};

  -- 发起交接准备；准备期间当前绑定不变
  Handover(target: NodeId, expected: Revision)
  ext rd revision wr prepare
  pre expected = revision and prepare = nil
  post prepare = mk_Prepare(target, expected);

  -- 交接生效边界；仍欠投递责任时不得更换处理者
  Commit()
  ext rd firstOwner rd acked rd expired wr assignee wr revision wr prepare
  pre prepare <> nil and prepare.expected = revision
      and dom firstOwner subset (acked union expired)
  post assignee = prepare~.target
       and revision = revision~ + 1
       and prepare = nil;

  -- 中止交接准备；绑定与版本保持原值
  Abort()
  ext wr prepare
  pre prepare <> nil
  post prepare = nil;

  -- 接收者确认已持久保存该事件；确认只清积压，不改归属
  Ack(e: EventId, actor: NodeId)
  ext rd firstOwner wr acked
  pre e in set dom firstOwner and actor = firstOwner(e)
  post acked = acked~ union {e};

  -- 保留期到达时终结投递义务；重复到达不产生新处置
  Expire(e: EventId)
  ext rd firstOwner wr expired
  pre e in set dom firstOwner
  post expired = expired~ union {e};

  -- 取事件的投送接收者；取首次归属
  Recipient(e: EventId) r: NodeId
  ext rd firstOwner
  pre e in set dom firstOwner
  post r = firstOwner(e);
end Binding
```
