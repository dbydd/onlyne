# 域成员连接

身份认证与事务提交由节点边界承担。候选声明经绑定身份的连接取得；遗漏候选或查询失败均不能证明节点已经退出。

```vdm
module Domain
imports
  from Identity types NodeId renamed NodeId
exports
  types DomainId; MembershipRevision; Outcome;
        struct Membership; struct Declaration; struct Discovery
  operations
    Create: NodeId * DomainId ==> Outcome;
    Join: NodeId * DomainId * [MembershipRevision] ==> Outcome;
    Leave: NodeId * DomainId * [MembershipRevision] ==> Outcome;
    Describe: DomainId ==> [Membership];
    Discover: DomainId * set of NodeId * map NodeId to [Declaration] ==> Discovery
 definitions
 types
  -- 无 owner 的节点组标识；只比较身份
  DomainId = token;
  -- 同一节点与同一域的版本；转换后取得新值
  MembershipRevision = nat1;
  -- 修改产生新关系、复用当前关系或拒绝；拒绝不修改事实
  Outcome = <Changed> | <Unchanged> | <Refused>;

  -- 本节点对一个域的当前声明；退出仍留版本围栏
  Membership ::
    node_id : NodeId  -- 声明者身份
    status : <Active> | <Left>  -- 本次关系的有效性
    revision : MembershipRevision;  -- 此关系当前版本

  -- 声明由被查询节点自己的认证连接取得，候选目录只提供身份
  Declaration ::
    domain_id : DomainId  -- 被查询的节点组
    membership : Membership;  -- 该节点自己的回答

  -- 本次核对取得的成员观察与失败身份；不宣称全局成员全集
  Discovery ::
    observed : set of Declaration  -- 已核对的 active 声明
    unavailable : set of NodeId;  -- 无回答或回答身份不匹配的候选

-- 成员关系面；每个实例只能写本节点的关系
state DomainState of
  self : NodeId  -- 本节点固定身份
  records : map DomainId to Membership  -- 各域当前关系，包含退出墓碑
  inv mk_DomainState(n, rs) ==
    (forall d in set dom rs & rs(d).node_id = n)
    and card {d | d in set dom rs & rs(d).status = <Active>} <= 1
  init s == s = mk_DomainState(mk_token(1), {|->})
end

operations
  -- 调用方提供新分配的域标识；创建者只写自己的成员记录
  Create(actor: NodeId, domain: DomainId) r: Outcome
  ext rd self wr records
  pre domain not in set dom records
  post if actor <> self or
          exists d in set dom records~ & records~(d).status = <Active>
       then r = <Refused> and records = records~
       else r = <Changed> and records = records~ ++
                {domain |-> mk_Membership(self, <Active>, 1)};

  -- 条件加入；已有其他 active 域或版本过时均拒绝
  Join(actor: NodeId, domain: DomainId, expected: [MembershipRevision]) r: Outcome
  ext rd self wr records
  post if actor <> self or
          (exists d in set dom records~ & d <> domain and records~(d).status = <Active>)
       then r = <Refused> and records = records~
       elseif domain not in set dom records~
       then if expected <> nil
            then r = <Refused> and records = records~
            else r = <Changed> and records = records~ ++
                     {domain |-> mk_Membership(self, <Active>, 1)}
       elseif expected <> records~(domain).revision
       then r = <Refused> and records = records~
       elseif records~(domain).status = <Active>
       then r = <Unchanged> and records = records~
       else r = <Changed> and records = records~ ++
                {domain |-> mk_Membership(self, <Active>, records~(domain).revision + 1)};

  -- 条件退出；墓碑保留到下一次有效加入
  Leave(actor: NodeId, domain: DomainId, expected: [MembershipRevision]) r: Outcome
  ext rd self wr records
  post if actor <> self or domain not in set dom records~
       then r = <Refused> and records = records~
       elseif expected <> records~(domain).revision
       then r = <Refused> and records = records~
       elseif records~(domain).status = <Left>
       then r = <Unchanged> and records = records~
       else r = <Changed> and records = records~ ++
                {domain |-> mk_Membership(self, <Left>, records~(domain).revision + 1)};

  -- 查询当前关系；nil 只表示本节点没有此域记录
  Describe(domain: DomainId) r: [Membership]
  ext rd records
  post r = (if domain in set dom records then records(domain) else nil);

  -- replies 缺键表示查询失败，nil 表示该节点明确答复没有记录
  -- 认证、超时和地址解析由网络边界处理；身份或域不符同样记为 unavailable
  Discover(domain: DomainId, candidates: set of NodeId,
           replies: map NodeId to [Declaration]) r: Discovery
  ext rd self rd records
  pre domain in set dom records and records(domain).status = <Active>
  post r.observed =
         {mk_Declaration(domain, records(domain))} union
         {replies(n) | n in set candidates \ {self} &
           n in set dom replies and replies(n) <> nil
           and replies(n).domain_id = domain
           and replies(n).membership.node_id = n
           and replies(n).membership.status = <Active>}
       and r.unavailable =
         {n | n in set candidates \ {self} &
           n not in set dom replies or
           (replies(n) <> nil and
             (replies(n).domain_id <> domain or replies(n).membership.node_id <> n))};
end Domain
```
