#!/usr/bin/env python3
"""Reproduce each open finding on its own: start from the cursor config, turn on the
one behaviour the finding needs, check, and print the rule TLC reports with the
steps of its shortest trace. A finding whose behaviour no longer breaks the rule
prints `fixed`: update ../SKILL.md and move that behaviour into the cursor config.
usage: findings.py"""
import os, re, subprocess, tempfile

here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = open(f"{here}/model/StageFeedback.tla").read()
cfg = open(f"{here}/model/StageFeedback-cursor.cfg").read()

FINDINGS = [
    ("`notes wait --post P` skips user comments on other posts", "WaitPost", "NoLoss"),
    ("a `notes wait` whose client died consumes the next comment", "Abandon", "NoLoss"),
    ("a reply lost after the cursor was written (daemon or socket dies) takes its comments with it", "ReplyLost", "NoLoss"),
    ("deleting a delivered comment, then restarting notesd, reuses its seq under agent_seq", "Deletes", "NoLoss"),
]

for name, constant, rule in FINDINGS:
    assert f"{constant} = FALSE" in cfg, f"cursor config no longer has {constant} = FALSE"
    d = tempfile.mkdtemp(prefix="stagefeedback")
    open(f"{d}/StageFeedback.tla", "w").write(spec)
    open(f"{d}/StageFeedback.cfg", "w").write(cfg.replace(f"{constant} = FALSE", f"{constant} = TRUE", 1))
    out = subprocess.run([f"{here}/scripts/check.sh", f"{d}/StageFeedback.tla"], capture_output=True, text=True).stdout
    steps = re.findall(r"^State \d+: <(\w+(?:\([^)]*\))?)", out, re.M)
    status = "open " if f"Invariant {rule} is violated" in out else "fixed"
    print(f"{status}  {rule:<7} {constant:<10} {name}")
    if steps:
        print(f"         {' -> '.join(steps[1:])}")
