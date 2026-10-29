#!/usr/bin/env python3
"""Show each rule failing on a regression: apply each mutation (written the way the
regression would look in the code, mapped into the model) to a copy of the model,
check it, and expect TLC to report the named rule. Prints one line per mutation;
exits 1 if any mutation goes uncaught. usage: mutations.py"""
import os, subprocess, sys, tempfile

here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = open(f"{here}/model/StagePixels.tla").read()
cfg = open(f"{here}/model/StagePixels.cfg").read()

MUTATIONS = [
    ("Gallery::park keeps the held previous", "OneToOne", [
        ("Parked(i) == IF i.previous # 0 THEN [i EXCEPT !.previous = 0, !.placed = FALSE] ELSE i", "Parked(i) == i"),
        ("/\\ dead' = IF park /\\ img.previous # 0 THEN dead \\cup {img.previous} ELSE dead", "/\\ dead' = dead"),
        ("dead2 == IF parkR /\\ img1.previous # 0 THEN dead1 \\cup {img1.previous} ELSE dead1", "dead2 == dead1")]),
    ("kitty::delete with d=i (only unplaces)", "NoLeak", [
        ("held2 == held1 \\ dead2", "held2 == held1")]),
    ("place's resize hold ignores OnScreen.px", "OneToOne", [
        ("/\\ onScreen /\\ visuals # Size(img2.made)", "/\\ onScreen")]),
    ("App::leave_focus doesn't restore the feed size", "OneToOne", [
        ("/\\ visuals' = IF unzoomed # None THEN unzoomed ELSE visuals", "/\\ visuals' = visuals")]),
    ("App::enter_card_focus ignores left_focus", "OneToOne", [
        ("/\\ visuals' = IF leftFocus # None THEN leftFocus", "/\\ visuals' = IF FALSE THEN leftFocus")]),
    ("retarget drops the zoom made ahead once the view moved", "OneToOne", [
        ("useP == takeP\n", "useP == takeP /\\ view = \"Z0\"\n")]),
    ("a pan replaces pixels without asking for the new view", "Crisp", [
        ("RT(f, w, v) == IF f THEN v ELSE w", "RT(f, w, v) == IF f THEN (IF v = \"ZB\" THEN \"ZA\" ELSE v) ELSE w")]),
]

missed = 0
for name, rule, edits in MUTATIONS:
    s = spec
    for a, b in edits:
        assert a in s, f"{name}: model no longer contains {a!r}"
        s = s.replace(a, b, 1)
    d = tempfile.mkdtemp(prefix="stagepixels")
    open(f"{d}/StagePixels.tla", "w").write(s)
    open(f"{d}/StagePixels.cfg", "w").write(cfg)
    out = subprocess.run([f"{here}/scripts/check.sh", f"{d}/StagePixels.tla"], capture_output=True, text=True).stdout
    caught = f"Invariant {rule} is violated" in out
    missed += not caught
    print(f"{'caught' if caught else 'MISSED'}  {rule:<9} {name}")
sys.exit(1 if missed else 0)
