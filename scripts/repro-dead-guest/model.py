#!/usr/bin/env python3
"""Monte Carlo of the reproduction scenario (no cluster needed).

Guest: STOP ~ U[stop_min, stop_min+jitter] s, then CONT for `cont` s, repeating;
the first STOP starts 5 s after the loop launch. The idle stage fires 95 s after
launch. An attempt holds a 180 s flush budget and succeeds if a CONT window
starts (or is open) inside it. NEW: backoff 10 s << (n-1), cap 600 s, park
after the 10th failure (next try +6 h). OLD: retry 5 s after each failure.
Prints P(suspended before park) for NEW and the OLD attempts/time distribution.
"""
import random
import statistics
import sys

STOP_MIN, JITTER, CONT, BUDGET = 1200, 3600, 2, 180
TRIALS = int(sys.argv[1]) if len(sys.argv) > 1 else 20000


def windows(rng, horizon):
    t, out = 5.0, []
    while t < horizon:
        t += rng.randint(STOP_MIN, STOP_MIN + JITTER)
        out.append(t)
        t += CONT
    return out


def attempt_ok(conts, start):
    return any(start - CONT <= c <= start + BUDGET for c in conts)


def run(rng, policy):
    conts = windows(rng, 40000)
    t, n = 95.0, 0
    while t < 30000:
        n += 1
        if attempt_ok(conts, t):
            return n, t - 95.0, True
        t += BUDGET
        if policy == "new":
            if n >= 10:
                return n, t - 95.0, False
            t += min(10 << min(n - 1, 6), 600)
        else:
            t += 5
    return n, t - 95.0, False


rng = random.Random(1)
new = [run(rng, "new") for _ in range(TRIALS)]
old = [run(rng, "old") for _ in range(TRIALS)]
p_suspend_new = sum(ok for _, _, ok in new) / TRIALS
print(f"NEW: P(suspended within the 10-attempt budget) = {p_suspend_new:.3f}; P(park) = {1 - p_suspend_new:.3f}")
attempts = [n for n, _, ok in old if ok]
minutes = [s / 60 for _, s, ok in old if ok]
print(f"OLD: P(suspended) = {len(attempts) / TRIALS:.3f}; attempts median {statistics.median(attempts):.0f} "
      f"p10 {sorted(attempts)[TRIALS // 10]} p90 {sorted(attempts)[9 * TRIALS // 10]}; "
      f"minutes median {statistics.median(minutes):.0f} max {max(minutes):.0f}; "
      f"P(attempts > 10) = {sum(a > 10 for a in attempts) / len(attempts):.3f}")
total = sum(n for n, _, _ in old) + sum(n for n, _, _ in new)
succ = sum(ok for _, _, ok in old) + sum(ok for _, _, ok in new)
print(f"per-attempt success (both policies pooled) = {succ / total:.3f}")
