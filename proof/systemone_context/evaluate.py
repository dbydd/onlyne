"""Run task-specific decision cases against the local group/decider route.

Each slice exports CASES and questions(case, revision). A case carries id, point,
split, provenance, profiles (minimal/focused/expanded), and expected answers.
Expected labels and provenance never enter model requests.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import random
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

ENDPOINT = "http://127.0.0.1:3425/v1/systemone"
MODEL = "group/decider"


def permuted(value, seed, field=""):
    if isinstance(value, dict):
        pairs = list(value.items())
        seed.shuffle(pairs)
        return {key: permuted(item, seed, key) for key, item in pairs}
    if isinstance(value, list):
        return [permuted(item, seed, field) for item in value]
    return value


def evaluate(module, case, profile, revision, order_seed=None):
    state = case["profiles"][profile]
    questions = module.questions(case, revision)
    if order_seed is not None:
        rng = random.Random(order_seed)
        state = permuted(state, rng)
        questions = permuted(questions, rng)
    if any(q["type"] == "choice" and len(q["criteria"]) < 2 for q in questions.values()):
        raise ValueError(f"{case['id']}: a choice requires competing options")
    if set(questions) != set(case["expected"]):
        raise ValueError(f"{case['id']}: question and label keys differ")
    request = {"model": MODEL, "state": state, "questions": questions}
    payload = json.dumps(request, ensure_ascii=False).encode()
    started = time.monotonic()
    response = None
    error = None
    attempts = 0
    attempt_records = []
    for attempt in range(2):
        attempts += 1
        response = None
        try:
            req = urllib.request.Request(ENDPOINT, data=payload, headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=120) as stream:
                raw = stream.read().decode()
                response = json.loads(raw)
            if not isinstance(response, dict) or not isinstance(response.get("answers"), dict):
                raise ValueError("response has no answer map")
            if set(response["answers"]) != set(questions):
                raise ValueError("response answer keys differ from questions")
            for key, question in questions.items():
                answer = response["answers"][key]
                if answer.get("type") != question["type"]:
                    raise ValueError(f"{key}: answer type mismatch")
                if question["type"] == "choice" and answer.get("choice") not in question["criteria"]:
                    raise ValueError(f"{key}: answer is not a declared choice")
                if question["type"] == "noul" and (type(answer.get("noul")) not in (int, float) or not 0 <= answer["noul"] <= 1):
                    raise ValueError(f"{key}: noul answer is not numeric")
            attempt_records.append({"attempt": attempts, "response": response})
            error = None
            break
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"
            attempt_records.append({"attempt": attempts, "error": error, "response": response})
    answers = response.get("answers", {}) if error is None else {}
    actual = {}
    for key, answer in answers.items():
        actual[key] = answer.get("choice") if answer.get("type") == "choice" else answer.get("noul", 0) >= 0.5
    checks = {key: actual.get(key) == value for key, value in case["expected"].items()}
    return {
        "id": case["id"], "point": case["point"], "split": case["split"],
        "profile": profile, "revision": revision, "order_seed": order_seed,
        "provenance": case["provenance"],
        "request": request, "request_sha256": hashlib.sha256(payload).hexdigest(),
        "response": response, "error": error, "attempts": attempts, "attempt_records": attempt_records,
        "elapsed_ms": round((time.monotonic() - started) * 1000),
        "expected": case["expected"], "actual": actual, "checks": checks,
        "plan_pass": error is None and all(checks.values()),
    }


def summarize(results):
    groups = {}
    for row in results:
        key = (row["point"], row["split"], row["profile"], row["revision"])
        group = groups.setdefault(key, {"cases": 0, "plans": 0, "questions": 0, "correct": 0,
                                        "errors": 0, "token_sum": 0, "token_count": 0,
                                        "elapsed_ms_sum": 0})
        group["cases"] += 1
        group["plans"] += int(row["plan_pass"])
        group["questions"] += len(row["checks"])
        group["correct"] += sum(row["checks"].values())
        group["errors"] += int(row["error"] is not None)
        group["elapsed_ms_sum"] += row["elapsed_ms"]
        usage = (row["response"] or {}).get("usage", {})
        tokens = usage.get("input_tokens")
        if tokens is not None:
            group["token_sum"] += tokens
            group["token_count"] += 1
    return [dict(point=k[0], split=k[1], profile=k[2], revision=k[3], **v) for k, v in sorted(groups.items())]


def run(modules, split, profiles, revision, workers=4, order_seed=None):
    jobs = []
    identifiers = set()
    sources = []
    for name in modules:
        module = importlib.import_module(f"cases.{name}")
        source = Path(module.__file__).read_bytes()
        sources.append({"module": name, "sha256": hashlib.sha256(source).hexdigest()})
        for case in module.CASES:
            if case["id"] in identifiers:
                raise ValueError(f"duplicate id: {case['id']}")
            identifiers.add(case["id"])
            if split != "all" and case["split"] != split:
                continue
            jobs.extend((module, case, profile, revision, order_seed) for profile in profiles)
    if not jobs:
        raise ValueError("selection contains no cases")
    with ThreadPoolExecutor(max_workers=workers) as pool:
        results = list(pool.map(lambda args: evaluate(*args), jobs))
    return {"at": datetime.now(timezone.utc).isoformat(), "endpoint": ENDPOINT,
            "route": MODEL, "sources": sources, "summary": summarize(results), "results": results}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--slices", nargs="+", required=True)
    parser.add_argument("--split", choices=["dev", "holdout", "all"], default="dev")
    parser.add_argument("--profiles", nargs="+", choices=["minimal", "focused", "expanded"], default=["focused"])
    parser.add_argument("--revision", choices=["initial", "tuned"], default="tuned")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--order-seed", type=int)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    if Path(args.output).exists():
        raise FileExistsError(f"refusing to overwrite prior evidence: {args.output}")
    report = run(args.slices, args.split, args.profiles, args.revision, args.workers, args.order_seed)
    target = Path(args.output)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"report": str(target), "summary": report["summary"],
                      "failures": [{"id": r["id"], "profile": r["profile"], "expected": r["expected"],
                                    "actual": r["actual"], "error": r["error"]}
                                   for r in report["results"] if not r["plan_pass"]]}, ensure_ascii=False, indent=2))
    return 1 if any(not row["plan_pass"] for row in report["results"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
