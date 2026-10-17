#!/usr/bin/env python3
"""Regression gate over criterion results.

The benches run with `--baseline-lenient main` and criterion writes the change against the saved
`main` baseline into `<bench>/change/estimates.json`. This script
  * divides every change by the change of the code-independent `calibrate` bench (shared CI runners
    differ in speed from run to run),
  * flags a bench only when even the optimistic end of its 95% interval is above the threshold,
  * re-runs the flagged benches once (a busy neighbour on one core hits single benches), and fails
    only if they regress again.
`--promote` copies the fresh results over the `main` baseline (run on main after the comparison).
Usage: bench-compare.py [threshold_percent=5] [--promote]
"""
import json, pathlib, re, shutil, subprocess, sys

PACKAGES = ["musubi-mesh", "musubi-proto", "musubi-auth"]
root = pathlib.Path("target/criterion")
args = [a for a in sys.argv[1:] if a != "--promote"]
threshold = float(args[0]) if args else 5.0
# Sub-100 ns differences are below what shared runners can resolve (CPU model, cache placement and
# code alignment move a 300 ns bench by ±25 % with no code change), so a bench must also move by at
# least this many nanoseconds before it counts as a regression.
floor_ns = float(args[1]) if len(args) > 1 else 100.0


def benches():
    return [f.parent.parent for f in root.glob("**/new/estimates.json")]


def bench_id(bench):
    return json.load(open(bench / "new" / "benchmark.json"))["full_id"]


def change(bench):
    f = bench / "change" / "estimates.json"
    if not f.exists():
        return None
    m = json.load(open(f))["mean"]
    return m["point_estimate"], m["confidence_interval"]["lower_bound"]


def compare():
    ref = change(root / "calibrate")
    scale = 1.0 + ref[0] if ref else 1.0
    if ref:
        print(f"machine factor from `calibrate`: {ref[0] * 100:+.2f}% (changes below are corrected by it)")
    rows, failing = [], []
    for bench in benches():
        if bench.name == "calibrate":
            continue
        ch = change(bench)
        if ch is None:
            continue
        point, low = ch
        pct = ((1.0 + point) / scale - 1.0) * 100.0
        low_pct = ((1.0 + low) / scale - 1.0) * 100.0
        now = json.load(open(bench / "new" / "estimates.json"))["mean"]["point_estimate"]
        before = now / (1.0 + point)
        rows.append((bench_id(bench), before, now, pct))
        if low_pct > threshold and now - before > floor_ns:
            failing.append(bench_id(bench))
    rows.sort(key=lambda r: -r[3])
    for name, before, now, pct in rows:
        note = "  (below noise floor)" if pct > threshold and now - before <= floor_ns else ""
        print(f"{pct:+7.2f}%  {before/1e3:10.2f} µs → {now/1e3:10.2f} µs  {name}{note}")
    if not rows:
        print("no `main` baseline to compare against (first run)")
    return failing


if "--promote" in sys.argv:
    for bench in benches():
        shutil.rmtree(bench / "main", ignore_errors=True)
        shutil.copytree(bench / "new", bench / "main")
    print(f"promoted {len(benches())} benches to the `main` baseline")
    sys.exit(0)

failing = compare()
if failing:
    print(f"\nre-running suspects: {', '.join(failing)}")
    pattern = "^(" + "|".join(re.escape(f) for f in failing) + ")$"
    cmd = ["cargo", "bench", *sum((["-p", p] for p in PACKAGES), []), "--benches", "--",
           "--noplot", "--baseline-lenient", "main", pattern]
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
    print()
    failing = compare()
if failing:
    print(f"REGRESSION (confirmed on re-run): {', '.join(failing)} slower than `main` by more than {threshold}%")
    sys.exit(1)
