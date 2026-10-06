#!/usr/bin/env python3
"""Experiment 1 stress variants, run between asi_workload.py phases.

  stress.py burst RUN_DIR --name NAME [--mem-mb N] [--target-node NODE] [--restart-node-stack]

For every sandbox in RUN_DIR (the <label>.json files asi_workload.py writes):
  1. optionally raise memhog's target to N MB and wait until it is resident;
  2. suspend all sandboxes concurrently and wait until each snapshot is portable;
  3. optionally replace the sandbox-worker node-stack pods (rollout restart) while
     everything is suspended, i.e. restore right after a node-stack restart;
  4. with --target-node, cordon every other sandbox worker so all restores land
     on one node; wake all sandboxes concurrently; uncordon.
Results: RUN_DIR/stress-NAME.jsonl. Afterwards resume the workload with
`asi_workload.py run LABEL --out RUN_DIR --attach --attach-phase ... --plan ''`.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import asi_workload as asi  # noqa: E402

KCTX = asi.KCTX


def iso():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def burst(args):
    run = Path(args.run_dir)
    out = open(run / f"stress-{args.name}.jsonl", "a")

    def log(**fields):
        fields["ts"] = iso()
        out.write(json.dumps(fields) + "\n")
        out.flush()
        print(json.dumps(fields), flush=True)

    labels = sorted(p.stem for p in run.glob("a*.json"))
    workers = {}
    for label in labels:
        a = argparse.Namespace(out=str(run), label=label)
        workers[label] = asi.Run(a)
    log(step="start", labels=labels, mem_mb=args.mem_mb, target=args.target_node,
        restart=args.restart_node_stack)

    if args.mem_mb:
        for label, w in workers.items():
            w.command("mem_target", f"echo {args.mem_mb} > /tmp/memtarget")
        deadline = time.time() + 900
        pending = set(labels)
        while pending and time.time() < deadline:
            for label in sorted(pending):
                ok, text = workers[label].command("mem_wait", "cat /tmp/memhog.mb; free -m | sed -n 2p")
                lines = text.split()
                if ok and lines and lines[0].isdigit() and int(lines[0]) >= args.mem_mb - 32:
                    pending.discard(label)
                    log(step="mem_reached", label=label, out=text.strip()[:120])
            time.sleep(10)
        log(step="mem_phase_done", not_reached=sorted(pending))

    sources = {label: w.pod_node() for label, w in workers.items()}
    procs = {label: subprocess.Popen([sys.executable, str(HERE / "driver.py"), "action",
                                      w.state["sandbox_id"], "suspend"],
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
             for label, w in workers.items()}
    for label, proc in procs.items():
        log(step="suspend", label=label, source=sources[label], result=proc.communicate()[0].strip()[-400:])
    t0 = time.time()
    pending = set(labels)
    while pending and time.time() - t0 < 900:
        for label in sorted(pending):
            if workers[label].portable():
                pending.discard(label)
        time.sleep(3)
    log(step="portable", waited_s=round(time.time() - t0, 1), not_portable=sorted(pending))

    if args.restart_node_stack:
        subprocess.run(KCTX + ["rollout", "restart", "ds/space-node-stack"], check=True)
        subprocess.run(KCTX + ["rollout", "status", "ds/space-node-stack", "--timeout=600s"], check=True)
        time.sleep(args.settle)
        log(step="node_stack_restarted")

    cordoned = [n for n in asi.WORKERS if args.target_node and n != args.target_node]
    for node in cordoned:
        subprocess.run(KCTX[:3] + ["cordon", node], check=True)
    try:
        t1 = time.time()
        procs = {label: subprocess.Popen([sys.executable, str(HERE / "driver.py"), "action",
                                          w.state["sandbox_id"], "wake", "--wait", "900"],
                                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                 for label, w in workers.items()}
        results = {label: proc.communicate()[0].strip()[-400:] for label, proc in procs.items()}
        wall = round(time.time() - t1, 1)
    finally:
        for node in cordoned:
            subprocess.run(KCTX[:3] + ["uncordon", node])
    for label, w in workers.items():
        landed = w.pod_node()
        w.state["node"] = landed
        w.save()
        log(step="restore", label=label, source=sources[label], landed=landed,
            cross_node=landed != sources[label], burst_wall_s=wall, result=results[label])
    log(step="done")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("burst")
    p.add_argument("run_dir")
    p.add_argument("--name", required=True)
    p.add_argument("--mem-mb", type=int, default=0)
    p.add_argument("--target-node")
    p.add_argument("--restart-node-stack", action="store_true")
    p.add_argument("--settle", type=int, default=60, help="seconds after the restart before waking")
    args = parser.parse_args()
    burst(args)


if __name__ == "__main__":
    main()
