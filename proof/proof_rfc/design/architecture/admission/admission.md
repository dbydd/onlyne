# 接纳连接

``Admission`Accept`` 先查原身份再评估当前权限，因此成员退出或权限变化不改变已记录的拒绝。`proof/proof_rfc/design/appendix/reference/admission_safety.py` 用穷举状态搜索验证序号不复用、同一身份不接纳两次。

```vdm
module Admission
exports
  types MessageRef; struct Remembered; struct AcceptOutcome
  operations
    Accept: MessageRef * token * bool ==> AcceptOutcome;
    Expire: MessageRef ==> ()
definitions
types
  -- 消息身份；发送方在首字节前确定，两个节点对同一消息给同一值
  MessageRef = token;

  -- 可重放的接纳结局；结局本身与消息条目的保留期分别管理
  Receipt = <Accepted> | <Refused>;

  -- 身份证据；清除结局后仍阻止同一身份再次执行
  Remembered ::
    digest : token  -- 不可变请求的身份摘要
    receipt : [Receipt];  -- nil 表示只剩墓碑

  -- 调用方必须区分原结局、身份冲突和无法重放的墓碑三种结局
  AcceptOutcome = Receipt | <Conflict> | <Expired>;

  -- 接纳面的数学状态；条目可清除，序号与身份证据保留
state Log of
  entries : inmap nat1 to MessageRef  -- 仍在保留期内的消息条目
  known : map MessageRef to Remembered  -- 原结局与永久身份墓碑
  nextSeq : nat1  -- 从未使用过的下一个序号
  inv mk_Log(es, ks, n) ==
    rng es subset dom ks
    and (forall position in set dom es & position < n)
  init r == r = mk_Log({|->}, {|->}, 1)
end

operations
  -- 先查原身份再评估当前权限；命中墓碑时禁止再次接纳
  Accept(ref: MessageRef, digest: token, allowed: bool) r: AcceptOutcome
  ext wr entries wr known wr nextSeq
  post if ref in set dom known~
       then entries = entries~ and nextSeq = nextSeq~ and known = known~
         and r = (if known~(ref).digest <> digest then <Conflict>
                  elseif known~(ref).receipt = nil then <Expired>
                  else known~(ref).receipt)
       else known = known~ ++ {ref |-> mk_Remembered(digest,
                         if allowed then <Accepted> else <Refused>)}
         and (if allowed
              then r = <Accepted> and entries = entries~ ++ {nextSeq~ |-> ref}
                   and nextSeq = nextSeq~ + 1
              else r = <Refused> and entries = entries~ and nextSeq = nextSeq~);

  -- 仅在结果保留窗口之后清除明细；身份与摘要保持
  Expire(ref: MessageRef)
  ext wr entries wr known
  pre ref in set dom known
  post entries = entries~ :-> {ref}
       and known = known~ ++ {ref |-> mk_Remembered(known~(ref).digest, nil)};
end Admission
```
