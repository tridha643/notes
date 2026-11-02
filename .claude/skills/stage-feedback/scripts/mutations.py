#!/usr/bin/env python3
"""Show each rule failing on a regression: apply each mutation (written the way the
regression would look in the code, mapped into the model) to a copy of the model,
check it against the cursor config, and expect TLC to report the named rule.
Prints one line per mutation; exits 1 if any mutation goes uncaught. usage: mutations.py"""
import os, subprocess, sys, tempfile

here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = open(f"{here}/model/StageFeedback.tla").read()
cfg = open(f"{here}/model/StageFeedback-cursor.cfg").read()

MUTATIONS = [
    ("collect_feedback takes comments at or past agent_seq (>=)", "Once", [
        ("{c \\in comments : c.seq > cursor}", "{c \\in comments : c.seq >= cursor}")]),
    ("post_new replies with the result built before collect_feedback (feedback: Vec::new())", "NoLoss", [
        ("hand |-> {c.n : c \\in {f \\in fresh : f.user}},", "hand |-> {},")]),
    ("next_user_comment returns the comment without moving agent_seq", "Once", [
        ("next |-> found.seq]}", "next |-> cursor]}")]),
    ("next_user_comment moves agent_seq to the newest user comment, not the one it returns", "NoLoss", [
        ("next |-> found.seq]}", "next |-> Max({c.seq : c \\in cands})]}")]),
    ("next_user_comment takes comments at or past agent_seq (>=)", "Once", [
        ("c.seq > cursor /\\ (f", "c.seq >= cursor /\\ (f")]),
    ("StageIndex::add_session stops tracking max_comment_seq (seqs restart at 1 after a restart)", "NoLoss", [
        ("maxSeq' = IF comments = {} THEN 0 ELSE Max({c.seq : c \\in comments})", "maxSeq' = 0")]),
    ("Daemon::unit stops taking the store lock (two threads read one cursor)", "Once", [
        ("Free == units = {}", "Free == TRUE")]),
]

missed = 0
for name, rule, edits in MUTATIONS:
    s = spec
    for a, b in edits:
        assert a in s, f"{name}: model no longer contains {a!r}"
        s = s.replace(a, b, 1)
    d = tempfile.mkdtemp(prefix="stagefeedback")
    open(f"{d}/StageFeedback.tla", "w").write(s)
    open(f"{d}/StageFeedback.cfg", "w").write(cfg)
    out = subprocess.run([f"{here}/scripts/check.sh", f"{d}/StageFeedback.tla"], capture_output=True, text=True).stdout
    caught = f"Invariant {rule} is violated" in out
    missed += not caught
    print(f"{'caught' if caught else 'MISSED'}  {rule:<7} {name}")
sys.exit(1 if missed else 0)
