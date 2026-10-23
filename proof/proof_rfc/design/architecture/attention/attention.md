# 关注连接

``Attention`Automatic`` 只由 ``Attention`AttentionState.queued`` 与 ``Attention`AttentionState.injected`` 决定。``Attention`AttentionState.acked`` 记录成员已经持久保存的项；确认不改变 ``Attention`AttentionState.due``。thread 关闭属于 RFC 0002 的投递规则，本面不持有它。

```vdm
module Attention
imports
  from Admission
    types MessageRef renamed MessageRef
exports
  types ThreadRef; ContextId; struct Item
  operations
    Admit: ThreadRef * MessageRef ==> ();
    Queue: ContextId * Item ==> ();
    Ack: ContextId * Item ==> ();
    Inject: ContextId * Item ==> ();
    Automatic: ContextId ==> set of Item
definitions
types
  -- thread 的身份；消息条目只归 owner 一个节点持有
  ThreadRef = token;

  -- 一次模型上下文；它自己的消费记录与另一个上下文互不推断
  ContextId = token;

  -- 一条要求指定成员主动关注的消息；它在存盘 thread 与关注面各出现一次
  Item ::
    thread : ThreadRef  -- 消息在 owner 处的存盘位置
    msg : MessageRef  -- 与该条目指向同一消息的身份

  -- 关注面：同一消息的三种推进阶段，消费完成不删除前两阶段的事实
state AttentionState of
  due : set of Item  -- 历次接纳产生的关注依据，不以消费完成删除
  queued : map ContextId to set of Item  -- 成员节点已持久接收的关注项
  acked : map ContextId to set of Item  -- 成员已持久保存正文或可恢复引用的项
  injected : map ContextId to set of Item  -- 已进入指定上下文的关注项
  inv mk_AttentionState(d, q, a, i) ==
    (forall ctx in set dom q & q(ctx) subset d)
    and (forall ctx in set dom a & ctx in set dom q and a(ctx) subset q(ctx))
    and (forall ctx in set dom i & ctx in set dom q and i(ctx) subset q(ctx))
  init r == r = mk_AttentionState({}, {|->}, {|->}, {|->})
end

operations
  -- 接纳一条消息时登记关注义务；重复接纳由上游去重，本面只认首次登记
  Admit(t: ThreadRef, m: MessageRef)
  ext wr due
  post due = due~ union {mk_Item(t, m)};

  -- 成员节点把一项关注写入本地持久队列；未经接纳的项不许进入
  Queue(ctx: ContextId, it: Item)
  ext rd due wr queued
  pre it in set due
  post queued = (if ctx in set dom queued~
                 then queued~ ++ {ctx |-> queued~(ctx) union {it}}
                 else queued~ ++ {ctx |-> {it}});

  -- 成员确认已经持久保存该条目的正文或可恢复引用；确认幂等
  Ack(ctx: ContextId, it: Item)
  ext rd queued wr acked
  pre ctx in set dom queued and it in set queued(ctx)
  post acked = (if ctx in set dom acked~
                then acked~ ++ {ctx |-> acked~(ctx) union {it}}
                else acked~ ++ {ctx |-> {it}});

  -- 记录关注项进入某个上下文；本地队列里没有的项不算已消费
  Inject(ctx: ContextId, it: Item)
  ext rd queued wr injected
  pre ctx in set dom queued and it in set queued(ctx)
  post injected = (if ctx in set dom injected~
                   then injected~ ++ {ctx |-> injected~(ctx) union {it}}
                   else injected~ ++ {ctx |-> {it}});

  -- 取该上下文仍需处理、且尚未消费的关注项；关闭 thread 不改变它
  Automatic(ctx: ContextId) r: set of Item
  ext rd queued rd injected
  post r = {it | it in set (if ctx in set dom queued then queued(ctx) else {}) &
                not (ctx in set dom injected and it in set injected(ctx))};
end Attention
```
