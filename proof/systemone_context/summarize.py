"""Summarize saved model requests without making model calls."""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import defaultdict
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("reports", nargs="+")
    parser.add_argument("--output")
    args = parser.parse_args()
    rows = []
    files = []
    for name in args.reports:
        path = Path(name)
        payload = path.read_bytes()
        data = json.loads(payload)
        rows.extend(data.get("results", []))
        files.append({"path": name, "sha256": hashlib.sha256(payload).hexdigest()})
    groups = defaultdict(list)
    by_case = defaultdict(dict)
    for row in rows:
        groups[(row["point"], row["split"], row["profile"], row["revision"])].append(row)
        by_case[(row["id"], row["split"], row["revision"])][row["profile"]] = row
    summary = []
    for key, values in sorted(groups.items()):
        usage = [(r.get("response") or {}).get("usage", {}).get("input_tokens") for r in values]
        tokens = [v for v in usage if v is not None]
        summary.append({"point": key[0], "split": key[1], "profile": key[2], "revision": key[3],
                        "cases": len(values), "plans": sum(v["plan_pass"] for v in values),
                        "questions": sum(len(v["checks"]) for v in values),
                        "correct": sum(sum(v["checks"].values()) for v in values),
                        "errors": sum(v.get("error") is not None for v in values),
                        "mean_input_tokens": round(sum(tokens) / len(tokens), 1) if tokens else None,
                        "mean_elapsed_ms": round(sum(v["elapsed_ms"] for v in values) / len(values), 1)})
    effects = []
    for (identifier, split, revision), profiles in sorted(by_case.items()):
        if "focused" in profiles and "minimal" in profiles:
            before, after = profiles["minimal"], profiles["focused"]
            if before["actual"] != after["actual"]:
                effects.append({"id": identifier, "split": split, "revision": revision,
                                "minimal": before["actual"], "focused": after["actual"],
                                "minimal_pass": before["plan_pass"], "focused_pass": after["plan_pass"]})
    result = {"files": files, "models": sorted({r["response"].get("model", "unknown") for r in rows if r.get("response")}),
              "requests": len(rows), "summary": summary, "context_effects": effects,
              "failures": [{"id": r["id"], "point": r["point"], "split": r["split"], "profile": r["profile"],
                            "expected": r["expected"], "actual": r["actual"], "error": r.get("error")}
                           for r in rows if not r["plan_pass"]]}
    if args.output:
        Path(args.output).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({k: result[k] for k in ("models", "requests", "summary")}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
