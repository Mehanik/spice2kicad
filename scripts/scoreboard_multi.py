#!/usr/bin/env python3
"""Multi-seed champion/challenger comparison (ADR-32).

The ADR-23 scoreboard reads ONE draw per arm from a stochastic optimiser.
ADR-31 measured the consequence: on `named_rails` the V16 bend spread is
sd = 1.57 over seeds 1..20, while the recorded arm-to-arm "regression"
that motivated a whole ADR was +5 -- a coincidence of one lucky draw
against one unlucky one. That ADR had to be retracted.

This aggregator reads k sinks per arm (see `just scoreboard-run-multi`)
and reports, per (metric, fixture) cell:

    champion mean +/- sd, challenger mean +/- sd, delta, Welch t

and classifies each cell as

    EFFECT      |t| > 2 and both arms measured on every seed
    unresolved  |t| <= 2 -- the spread swamps the difference
    inert       sd = 0 on BOTH arms; the delta is a real measurement,
                not a sample (7 of 18 fixtures are seed-inert on V16)

The distinction that matters: an `inert` zero is evidence, an
`unresolved` zero is an absence of evidence. Reporting them the same way
is exactly the failure this script exists to prevent.

Usage:
    scoreboard_multi.py <champ_dir> <chal_dir> <champ_name> <chal_name>

Each <dir> holds seed-N/ subdirectories of `metric<TAB>fixture<TAB>value`
sink files.
"""

import math
import os
import sys
from collections import defaultdict


def read_sink_dir(path):
    """One seed's sink dir -> {(metric, fixture): value}."""
    out = {}
    for name in os.listdir(path):
        f = os.path.join(path, name)
        if not os.path.isfile(f):
            continue
        with open(f, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                parts = line.rstrip("\n").split("\t")
                if len(parts) != 3:
                    continue
                metric, fixture, value = parts
                try:
                    out[(metric, fixture)] = float(value)
                except ValueError:
                    pass
    return out


def read_arm(root):
    """Arm dir -> {(metric, fixture): [value per seed]}."""
    seeds = sorted(
        d for d in os.listdir(root)
        if d.startswith("seed-") and os.path.isdir(os.path.join(root, d))
    )
    if not seeds:
        sys.exit(f"error: {root} holds no seed-N/ subdirectories -- "
                 f"run `just scoreboard-run-multi` first")
    acc = defaultdict(list)
    for s in seeds:
        for key, value in read_sink_dir(os.path.join(root, s)).items():
            acc[key].append(value)
    return acc, len(seeds)


def mean(v):
    return sum(v) / len(v)


def sd(v):
    if len(v) < 2:
        return 0.0
    m = mean(v)
    return math.sqrt(sum((x - m) ** 2 for x in v) / (len(v) - 1))


def main():
    if len(sys.argv) != 5:
        sys.exit(__doc__)
    champ_dir, chal_dir, champ_name, chal_name = sys.argv[1:5]
    champ, kc = read_arm(champ_dir)
    chal, kk = read_arm(chal_dir)

    print(f"Multi-seed scoreboard: {champ_name} (k={kc}) vs "
          f"{chal_name} (k={kk})")
    print("=" * 100)

    effects, unresolved, inert, partial = [], [], [], []
    for key in sorted(set(champ) | set(chal)):
        a, b = champ.get(key, []), chal.get(key, [])
        if not a or not b:
            partial.append((key, len(a), len(b)))
            continue
        # A cell must be measured on EVERY seed of both arms, or its mean
        # is over a different population than its partner's.
        if len(a) != kc or len(b) != kk:
            partial.append((key, len(a), len(b)))
            continue
        ma, mb, sa, sb = mean(a), mean(b), sd(a), sd(b)
        delta = mb - ma
        if sa == 0.0 and sb == 0.0:
            (inert if delta != 0.0 else inert).append((key, ma, mb, delta))
            continue
        se = math.sqrt(sa * sa / len(a) + sb * sb / len(b))
        t = delta / se if se > 0 else 0.0
        row = (key, ma, sa, mb, sb, delta, t)
        (effects if abs(t) > 2.0 else unresolved).append(row)

    def cell(k):
        return f"{k[0]:<28} {k[1]:<22}"

    print(f"\nEFFECTS (|t| > 2) -- {len(effects)} cell(s)")
    if not effects:
        print("  none: no cell's arm-to-arm difference clears its own "
              "seed spread")
    for key, ma, sa, mb, sb, d, t in sorted(effects, key=lambda r: -abs(r[6])):
        sign = "challenger better" if d < 0 else "challenger worse"
        print(f"  {cell(key)} {ma:8.3f}+/-{sa:6.3f}  {mb:8.3f}+/-{sb:6.3f}  "
              f"d={d:+8.3f}  t={t:+6.2f}  {sign}")

    print(f"\nINERT (sd = 0 on both arms; delta is a measurement, not a "
          f"sample) -- {len(inert)} cell(s)")
    moved = [r for r in inert if r[3] != 0.0]
    print(f"  {len(inert) - len(moved)} unchanged, {len(moved)} moved")
    for key, ma, mb, d in sorted(moved, key=lambda r: -abs(r[3])):
        print(f"  {cell(key)} {ma:8.3f} -> {mb:8.3f}  d={d:+8.3f}")

    print(f"\nUNRESOLVED (|t| <= 2; spread swamps the difference) -- "
          f"{len(unresolved)} cell(s)")
    shown = sorted(unresolved, key=lambda r: -abs(r[5]))[:12]
    for key, ma, sa, mb, sb, d, t in shown:
        print(f"  {cell(key)} {ma:8.3f}+/-{sa:6.3f}  {mb:8.3f}+/-{sb:6.3f}  "
              f"d={d:+8.3f}  t={t:+6.2f}")
    if len(unresolved) > len(shown):
        print(f"  ... and {len(unresolved) - len(shown)} more")

    if partial:
        print(f"\nPARTIAL (not measured on every seed -- NOT compared) -- "
              f"{len(partial)} cell(s)")
        for key, na, nb in partial[:12]:
            print(f"  {cell(key)} champion {na}/{kc} seeds, "
                  f"challenger {nb}/{kk} seeds")
        if len(partial) > 12:
            print(f"  ... and {len(partial) - 12} more")

    print("\n" + "=" * 100)
    print("Reading this: an INERT zero is evidence of no change. An "
          "UNRESOLVED zero is\nan absence of evidence. They are not the "
          "same, and the single-sample\nscoreboard could not tell them "
          "apart (ADR-31).")
    print("\nThis instrument does NOT issue a promotion verdict. ADR-23's "
          "rule is stated\nover the single-sample aggregate; use "
          "`just scoreboard` for the verdict and\nthis for whether the "
          "verdict rests on anything.")


if __name__ == "__main__":
    main()
