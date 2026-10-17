#!/usr/bin/env python3
"""Fail when any criterion bench regressed against its baseline.

criterion compares every plain run against the previous one (`base/`, restored from the Actions cache
on CI) and stores the relative change in `<bench>/change/estimates.json`; afterwards `new` becomes the
next `base`. Usage: bench-compare.py [threshold_percent=5].
"""
import json, pathlib, sys

threshold = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
root = pathlib.Path("target/criterion")
worst, rows = 0.0, []
for change in root.glob("**/change/estimates.json"):
    bench = change.parent.parent
    mean = json.load(open(change))["mean"]
    pct = mean["point_estimate"] * 100.0
    # regression only when even the optimistic end of the 95% interval is above the threshold,
    # so runner noise on a single sample does not fail the build
    low = mean["confidence_interval"]["lower_bound"] * 100.0
    now = json.load(open(bench / "new" / "estimates.json"))["mean"]["point_estimate"]
    worst = max(worst, low)
    rows.append((str(bench.relative_to(root)), now / (1.0 + pct / 100.0), now, pct))
rows.sort(key=lambda r: -r[3])
for name, before, now, pct in rows:
    print(f"{pct:+7.2f}%  {before/1e3:10.2f} µs → {now/1e3:10.2f} µs  {name}")
if not rows:
    print("no baseline to compare against (first run)")
elif worst > threshold:
    print(f"REGRESSION: lower bound of change {worst:.2f}% > {threshold}%")
    sys.exit(1)
