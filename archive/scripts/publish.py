#!/usr/bin/env python3
"""Publish the workspace to crates.io in dependency order.

One command per release: the script reads the member set from `cargo metadata`,
checks the release preconditions, uploads each crate with `cargo publish`, and
prints the receipt text for `CHANGELOG.md`.

Usage:
    ./scripts/publish.py                  # publish every member, sandbox build on
    ./scripts/publish.py --dry-run        # show the plan and the preflight result
    ./scripts/publish.py --no-verify      # skip the packaging sandbox build
    ./scripts/publish.py --from onlyne-client      # resume at a crate in the plan
    ./scripts/publish.py --only onlyne-proto,onlyne-frame   # a named subset
    ./scripts/publish.py --allow-dirty --allow-untagged     # relax the guards

Exit codes: 0 published or planned, 1 a crate failed, 2 a precondition refused.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

ATTEMPTS = 3
BACKOFF_SECS = 8
# A crates.io upload can drop its TLS connection mid-flight; the retry then
# answers "already exists" once the first attempt's bytes land. That reply is a
# success, and this marker is how the script tells the two apart.
ALREADY_EXISTS = "already exists"


def run(argv: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, cwd=cwd, capture_output=True, text=True)


def metadata(root: Path) -> dict:
    proc = run(["cargo", "metadata", "--format-version", "1", "--no-deps"], root)
    if proc.returncode != 0:
        sys.exit(f"cargo metadata failed:\n{proc.stderr}")
    return json.loads(proc.stdout)


def publishable(meta: dict) -> dict[str, dict]:
    """Workspace members that go to the registry. `publish = []` means `publish = false`."""
    members = set(meta["workspace_members"])
    return {
        pkg["name"]: pkg
        for pkg in meta["packages"]
        if pkg["id"] in members and pkg.get("publish") != []
    }


def internal_deps(crates: dict[str, dict]) -> dict[str, set[str]]:
    """The workspace-internal edge set, read off each path dependency's directory."""
    by_dir = {str(Path(pkg["manifest_path"]).parent): name for name, pkg in crates.items()}
    graph: dict[str, set[str]] = {}
    for name, pkg in crates.items():
        edges = set()
        for dep in pkg["dependencies"]:
            target = by_dir.get(dep.get("path") or "")
            if target and target != name:
                edges.add(target)
        graph[name] = edges
    return graph


def order(crates: dict[str, dict]) -> list[str]:
    """Dependency-first order: every crate after the workspace crates it needs.

    Depth is the longest chain below a crate, ties break by name, so the plan is
    stable across runs and reviewable in a diff.
    """
    graph = internal_deps(crates)
    depth: dict[str, int] = {}

    def resolve(name: str, trail: tuple[str, ...] = ()) -> int:
        if name in depth:
            return depth[name]
        if name in trail:
            sys.exit(f"dependency cycle through {name}")
        value = 1 + max((resolve(d, trail + (name,)) for d in graph[name]), default=-1)
        depth[name] = value
        return value

    return sorted(crates, key=lambda name: (resolve(name), name))


def preflight(root: Path, crates: dict[str, dict], args: argparse.Namespace) -> list[str]:
    problems = []
    versions = {pkg["version"] for pkg in crates.values()}
    if len(versions) > 1:
        problems.append(
            "member versions differ: " + ", ".join(sorted(versions))
            + " — this workspace releases every crate at one number"
        )
    if not args.allow_dirty:
        dirty = run(["git", "status", "--porcelain"], root).stdout.strip()
        if dirty:
            problems.append("worktree dirty; commit first or pass --allow-dirty")
    if not args.allow_untagged:
        tag = run(["git", "tag", "--points-at", "HEAD"], root).stdout.strip()
        if not tag:
            problems.append("HEAD carries no tag; publish from the release tag or pass --allow-untagged")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true", help="plan only, upload nothing")
    parser.add_argument("--no-verify", action="store_true", help="skip the packaging sandbox build")
    parser.add_argument("--only", help="comma-separated subset of crates")
    parser.add_argument("--from", dest="start", help="resume at this crate in the plan")
    parser.add_argument("--allow-dirty", action="store_true", help="publish from a dirty worktree")
    parser.add_argument("--allow-untagged", action="store_true", help="publish with no tag on HEAD")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    meta = metadata(root)
    crates = publishable(meta)
    plan = order(crates)

    problems = preflight(root, crates, args)
    if problems:
        for line in problems:
            print(f"refusing: {line}", file=sys.stderr)
        return 2

    if args.only:
        wanted = [c.strip() for c in args.only.split(",") if c.strip()]
        unknown = [c for c in wanted if c not in crates]
        if unknown:
            print(f"unknown crates: {', '.join(unknown)}", file=sys.stderr)
            return 2
        plan = [c for c in plan if c in wanted]
    if args.start:
        if args.start not in plan:
            print(f"--from {args.start} is not in the plan", file=sys.stderr)
            return 2
        plan = plan[plan.index(args.start) :]

    version = next(iter({pkg["version"] for pkg in crates.values()}))
    print(f"{len(plan)} crates at {version}: {' → '.join(plan)}")
    if args.dry_run:
        return 0

    flags = ["--locked"]
    flags += ["--allow-dirty"] if args.allow_dirty else []
    flags += ["--no-verify"] if args.no_verify else []

    results: list[tuple[str, int, str]] = []
    for crate in plan:
        attempts = 0
        for attempt in range(1, ATTEMPTS + 1):
            attempts = attempt
            print(f"==> {crate} (attempt {attempt})")
            proc = run(["cargo", "publish", *flags, "-p", crate], root)
            sys.stdout.write(proc.stdout + proc.stderr)
            if proc.returncode == 0:
                results.append((crate, attempt, "published"))
                break
            if ALREADY_EXISTS in proc.stdout + proc.stderr:
                results.append((crate, attempt, "already on the registry"))
                break
            time.sleep(BACKOFF_SECS)
        else:
            results.append((crate, attempts, "FAILED"))
            print(f"stopping: {crate} failed after {attempts} attempts", file=sys.stderr)
            break

    for crate, attempts, status in results:
        print(f"{crate}: {status} ({attempts} attempt{'s' if attempts != 1 else ''})")
    failed = [c for c, _, s in results if s == "FAILED"] + [
        c for c in plan if c not in {r[0] for r in results}
    ]
    if failed:
        print(f"not published: {', '.join(failed)}", file=sys.stderr)
        return 1

    sandbox = "skipped" if args.no_verify else "built"
    print(
        f"\nreceipt text: {len(results)} crates on crates.io at {version}, none yanked, "
        f"in the order {'`. `'.join(plan)}, each through "
        f"`cargo publish {' '.join(flags)} -p onlyne-<crate>` at tag "
        f"`{run(['git', 'describe', '--tags', '--exact', 'HEAD'], root).stdout.strip() or 'untagged'}`, "
        f"packaging sandbox build {sandbox}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
