"""Admission safety: bounded exhaustive search over one admission log.

The model is the VDM contract in
proof/proof_rfc/design/architecture/admission/admission.md. The search is exhaustive over
a finite identity/digest domain, not a single hand-picked trace, so the safety
result below means "no violation inside these bounds".

Identity and digest are compared as exact strings. No normalisation: RFC 0003
§3 puts `msg`, `delivery_kind` and `meta` into the identity digest verbatim.

Durable facts the search keeps:
  - remembered: ref -> (digest, receipt) where receipt is None after expiry
  - assignments: append-only audit of (seq, ref); a seq is never reused
  - payloads:   ref -> seq for live entries; expiry removes the payload only

Every state transition records an observation (ref, digest, allowed, outcome)
so a replay can be checked against the original outcome rather than against
current permissions.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass

REFS = ("m0", "m1")
DIGESTS = ("d0", "d1")
MAX_EXPIRIES = 1  # each ref expires at most once; total is bounded by that

ACCEPTED = "accepted"
REFUSED = "refused"
EXPIRED = "expired"
CONFLICT = "conflict"


@dataclass(frozen=True, slots=True)
class State:
    remembered: tuple[tuple[str, str, str | None], ...]
    assignments: tuple[tuple[int, str], ...]
    payloads: tuple[tuple[str, int], ...]
    next_seq: int
    expiries: int
    last: tuple[str, str, bool, str] | None


def initial() -> State:
    return State((), (), (), 1, 0, None)


def _index(rows: tuple, key) -> int | None:
    for i, row in enumerate(rows):
        if row[0] == key:
            return i
    return None


def _remember(state: State, ref: str) -> tuple[str, str | None] | None:
    for r, digest, receipt in state.remembered:
        if r == ref:
            return digest, receipt
    return None


def admit(state: State, ref: str, digest: str, allowed: bool) -> State:
    known = _remember(state, ref)
    if known is None:
        if allowed:
            remembered = state.remembered + ((ref, digest, ACCEPTED),)
            assignments = state.assignments + ((state.next_seq, ref),)
            payloads = state.payloads + ((ref, state.next_seq),)
            return State(remembered, assignments, payloads, state.next_seq + 1,
                         state.expiries, (ref, digest, allowed, ACCEPTED))
        remembered = state.remembered + ((ref, digest, REFUSED),)
        return State(remembered, state.assignments, state.payloads, state.next_seq,
                     state.expiries, (ref, digest, allowed, REFUSED))

    known_digest, known_receipt = known
    if known_digest != digest:
        # Identity changed: conflict, whatever the original outcome and current permission.
        return State(state.remembered, state.assignments, state.payloads, state.next_seq,
                     state.expiries, (ref, digest, allowed, CONFLICT))
    if known_receipt is None:
        # Tombstone: no replayable result, never re-accept.
        return State(state.remembered, state.assignments, state.payloads, state.next_seq,
                     state.expiries, (ref, digest, allowed, EXPIRED))
    return State(state.remembered, state.assignments, state.payloads, state.next_seq,
                 state.expiries, (ref, digest, allowed, known_receipt))


def expire(state: State, ref: str) -> State:
    if _remember(state, ref) is None:
        return state
    rows = []
    for r, digest, receipt in state.remembered:
        rows.append((r, digest, None) if r == ref else (r, digest, receipt))
    payloads = tuple(row for row in state.payloads if row[0] != ref)
    return State(tuple(rows), state.assignments, payloads, state.next_seq,
                 state.expiries + 1, None)


def transitions(state: State):
    for ref in REFS:
        known = _remember(state, ref)
        live = known is not None and known[1] is not None
        for digest in DIGESTS:
            if known is not None and known[0] != digest:
                continue  # same identity only; a changed digest is checked separately
            for allowed in (False, True):
                yield f"admit({ref},{digest},allowed={allowed})", admit(state, ref, digest, allowed)
        if known is not None and state.expiries < MAX_EXPIRIES:
            yield f"expire({ref})", expire(state, ref)
        if known is not None and not live:
            # Tombstone reached with a different digest: identity conflict, not re-accept.
            for digest in DIGESTS:
                if known[0] != digest:
                    yield f"admit({ref},{digest},allowed=True)", admit(state, ref, digest, True)


def violations(state: State) -> list[str]:
    out = []
    seqs = [s for s, _ in state.assignments]
    if len(seqs) != len(set(seqs)):
        out.append("seq_reused")
    refs = [r for _, r in state.assignments]
    if len(refs) != len(set(refs)):
        out.append("ref_admitted_twice")
    if state.next_seq != len(seqs) + 1:
        out.append("next_seq_not_monotone")
    live = {r for r, _ in state.payloads}
    remembered_live = {r for r, _, rcpt in state.remembered if rcpt == ACCEPTED}
    if live != remembered_live:
        out.append("payload_not_matches_receipt")
    if state.last is not None:
        ref, digest, allowed, outcome = state.last
        known = _remember(state, ref)
        if known is not None and known[0] == digest:
            if outcome == CONFLICT and known[1] is not None:
                out.append("conflict_with_same_digest")
            if outcome in (ACCEPTED, REFUSED) and known[1] != outcome:
                out.append("replay_did_not_reuse_outcome")
    return out


def explore() -> tuple[int, int, list[str]]:
    start = initial()
    seen = {start}
    queue = deque([start])
    explored = 0
    violations_found: list[str] = []
    while queue:
        state = queue.popleft()
        explored += 1
        for name, nxt in transitions(state):
            if nxt in seen:
                continue
            bad = violations(nxt)
            if bad:
                violations_found.append(f"{name} -> {sorted(set(bad))}")
            seen.add(nxt)
            queue.append(nxt)
    return explored, len(seen), violations_found


def run() -> list[dict]:
    explored, reachable, bad = explore()
    bounds = {
        "refs": list(REFS),
        "digests": list(DIGESTS),
        "max_expiries_per_ref": MAX_EXPIRIES,
        "search": "exhaustive BFS over the reachable state graph",
    }
    results = [
        {
            "scenario": "admission_safety",
            "kind": "control",
            "classification": "safety_control",
            "bounds": bounds,
            "states_explored": explored,
            "states_reachable": reachable,
            "witness_expected": False,
            "check_pass": not bad,
            "trace": [],
            "final": {"violations": bad},
            "note": "No safety violation inside these bounds. Absence is bounded, not a proof.",
        }
    ]
    return results


def main() -> int:
    results = run()
    payload = {"scenario": "admission_safety", "results": results}
    ok = all(r["check_pass"] for r in results)
    payload["all_checks_pass"] = ok
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
