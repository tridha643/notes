#!/usr/bin/env python3
"""Generate an isolated vault, drive one resident library process, and retain evidence."""
import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import random
import re
import select
import signal
import subprocess
import tempfile
import time

TOPICS = [
    ("database-migration", "database migration", "Keep schema changes backward compatible until every reader has moved."),
    ("meeting-notes", "meeting agenda", "The team agreed to share decisions with the next review group."),
    ("release-plan", "release checklist", "Ship a small cohort first and keep the rollback procedure nearby."),
    ("research-paper", "research findings", "The experiment compares independent observations against the baseline."),
    ("travel-itinerary", "train reservation", "Confirm the station and allow extra time for the connection."),
    ("recipe-sourdough", "bread fermentation", "Rest the dough overnight before shaping and baking the next morning."),
    ("project-orion", "Orion launch", "The next milestone joins the local editor with the resident service."),
    ("weekly-review", "weekly retrospective", "Carry unfinished actions into the next planning session."),
    ("api-design", "API contract", "Clients send one JSON object per line and receive an explicit version."),
    ("db-backup", "backup recovery", "Restore a recent snapshot into a separate directory before checking it."),
    ("performance-budget", "latency budget", "Separate indexing work from the latency experienced by an interactive search."),
    ("book-highlights", "reading journal", "Write down the argument and connect it to one concrete example."),
    ("garden-log", "seed germination", "Check the soil before watering and record when the first shoots appear."),
    ("invoice-audit", "invoice reconciliation", "Compare the receipt against the purchase order and the monthly statement."),
    ("incident-review", "incident response", "Preserve the timeline and identify which change restored service."),
    ("design-sketch", "sidebar layout", "The reading pane stays opaque while the sidebar uses a glass material."),
    ("rust-ownership", "borrow checker", "Keep ownership at the boundary and pass borrowed values through the hot path."),
    ("macos-watcher", "file watcher", "External atomic saves should replace the visible contents without a manual refresh."),
    ("two-character-ui", "UI navigation", "Small title rows leave enough room to scan the folder hierarchy."),
    ("archived-decision", "decision record", "Record the alternative and the condition that would change this decision."),
]
FOLDERS = [f"{area}/{section}" for area in ("work", "personal", "learning", "projects", "archive")
           for section in ("daily", "plans", "meetings", "reading", "ideas", "logs", "reference", "drafts", "reviews")]


def generate_vault(vault):
    rng = random.Random(249)
    files, sizes = [], []
    for i in range(10_000):
        slug, phrase, sentence = TOPICS[i % len(TOPICS)]
        relative = f"{FOLDERS[i % len(FOLDERS)]}/{slug}-{i:05d}.md"
        path = vault / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        size = rng.randint(1024, 20 * 1024)
        body = f"# {slug.replace('-', ' ').title()} {i}\n\nNote identity: noteidentity{i:05d}\n\n## Summary\n{phrase}. {sentence}\n\n"
        paragraph = 0
        while len(body.encode()) < size:
            _, other_phrase, other_sentence = rng.choice(TOPICS)
            # Different paragraphs, links, lists and numbers; no single repeated padding block.
            body += (f"## Observation {paragraph}\n" if paragraph % 5 == 0 else "")
            body += f"{rng.choice(['- Follow up:', 'Review:', 'Observation:', 'Next action:'])} {other_sentence} "
            body += f"Discuss {other_phrase} with group {rng.randrange(90)} in week {rng.randrange(1, 53)}. "
            body += f"See [[{rng.choice(TOPICS)[0]}-{rng.randrange(10000):05d}]] for context.\n\n"
            paragraph += 1
        body = body.encode()[:size - 1] + b"\n"
        path.write_bytes(body)
        files.append(relative)
        sizes.append(len(body))
    return {"files": len(files), "folders": 50, "leaf_folders": 45, "bytes": sum(sizes),
            "min_bytes": min(sizes), "max_bytes": max(sizes), "seed": 249, "first_paths": files[:20]}


def query_cases():
    find = ["database-migration", "databse-migrtion", "meeting-notes", "meting-notes", "release-plan",
            "research-paper", "travel-itinerary", "recipe-sourdough", "project-orion", "weekly-review",
            "api-design", "db-backup", "performance-budget", "book-highlights", "garden-log",
            "invoice-audit", "macos-watcher", "ui", "db", "zzzzunfindable999"]
    find_topics = [0, 0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18, 9, None]
    search = ["database migration", "meeting agenda", "release checklist", "research findings", "train reservation",
              "bread fermentation", "Orion launch", "weekly retrospective", "API contract", "backup recovery",
              "latency budget", "noteidentity09999", "seed germination", "invoice reconciliation", r"Observation\s+[0-9]+",
              "borrow checker", "fermenttaion", "UI", "db", "zzzzunfindable999"]
    cases = [{"kind": "find", "query": q, "expected_slug": TOPICS[t][0] if t is not None else None}
             for q, t in zip(find, find_topics)]
    for q in search:
        cases.append({"kind": "search", "query": q, "enforce_budget": False})
    return cases


def read_reply(process, timeout=300):
    ready, _, _ = select.select([process.stdout], [], [], timeout)
    if not ready:
        raise RuntimeError(f"fff-check driver: no reply within {timeout}s")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(f"fff-check driver: process exited {process.poll()}")
    return json.loads(line)


def request(process, payload):
    process.stdin.write(json.dumps(payload) + "\n")
    process.stdin.flush()
    return read_reply(process)


def capture_command(output, args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=50)
    output.write_text("\n".join(line.rstrip() for line in result.stdout.splitlines()) + "\n")
    return {"command": args, "exit_code": result.returncode, "artifact": output.name}


def check_watcher(process, vault, repeats, corpus, manual=False):
    observations = []
    for i in range(repeats):
        created_relative = f"work/daily/{'manual' if manual else 'watch'}probeunique{i:03d}.md"
        for action in ("create", "edit", "delete"):
            relative = corpus["first_paths"][i] if action == "edit" else created_relative
            path = vault / relative
            old = f"{'edit' if manual else 'note'}identity{i:05d}" if action == "edit" else f"watchsentineloldunique{i:03d}"
            new = f"{'done' if manual else 'edit'}identity{i:05d}" if action == "edit" else f"watchsentinelnewunique{i:03d}"
            if action == "edit":
                original = path.read_text()
                prime = request(process, {"op": "query", "kind": "search", "query": old, "scope": relative})
                assert any(m["path"] == relative for m in prime["matches"]), prime
            if not manual:
                request(process, {"op": "events"})
            start = time.perf_counter()
            if action == "create":
                path.write_text(f"# Watch probe\n{old}\n")
            elif action == "edit":
                # Atomic same-size save: a common editor behavior and a harder invalidation case.
                replacement = path.with_suffix(".tmp")
                replacement.write_text(original.replace(old, new))
                replacement.replace(path)
            else:
                path.unlink()
            mutation_ms = (time.perf_counter() - start) * 1000
            update = request(process, {"op": "update", "path": relative}) if manual else None
            update_ms = (time.perf_counter() - start) * 1000 if manual else None
            path_ms = content_ms = None
            event_ms = update_ms
            event_kinds = []
            polls = 0
            while time.perf_counter() - start < 10:
                paths = request(process, {"op": "query", "kind": "find", "query": path.stem})["paths"]
                present = relative in paths
                if path_ms is None and present == (action != "delete"):
                    path_ms = (time.perf_counter() - start) * 1000
                contents = []
                for sentinel in (old, new):
                    reply = request(process, {"op": "query", "kind": "search", "query": sentinel, "scope": relative})
                    contents.append(any(m["path"] == relative and sentinel in m["text"] for m in reply["matches"]))
                expected = {"create": [True, False], "edit": [False, True], "delete": [False, False]}[action]
                if content_ms is None and contents == expected:
                    content_ms = (time.perf_counter() - start) * 1000
                events = [] if manual else request(process, {"op": "events"})["events"]
                for event in events:
                    if event["path"] == str(path) and event["kind"] in {
                        "create": {"created_or_modified"}, "edit": {"created_or_modified"},
                        "delete": {"removed"}}[action]:
                        event_kinds.append(event["kind"])
                        if event_ms is None:
                            event_ms = (time.perf_counter() - start) * 1000
                polls += 1
                if path_ms is not None and content_ms is not None and event_ms is not None:
                    break
                time.sleep(0.01)
            observations.append({"trial": i, "action": action, "mutation_ms": mutation_ms,
                                 "path_ms": path_ms, "content_ms": content_ms, "event_ms": event_ms,
                                 "event_kinds": event_kinds, "polls": polls, "manual_update": update,
                                 "passed": path_ms is not None and content_ms is not None and event_ms is not None})
    return observations


def summarize_bench(bench):
    summary, checks = {}, []
    for kind in ("find", "search"):
        rows = [row for row in bench["queries"] if row["request"]["kind"] == kind]
        samples = sorted(s for row in rows for s in row["samples_ms"])
        summary[kind] = {"samples": len(samples), "p50_ms": samples[math.ceil(len(samples)*.50)-1],
                         "p95_ms": samples[math.ceil(len(samples)*.95)-1], "max_ms": samples[-1]}
        for row in rows:
            q, example = row["request"], row["example"]
            if kind == "find":
                slug = q["expected_slug"]
                passed = any(slug in p for p in example["paths"]) if slug else not example["paths"]
            else:
                needle = q["query"]
                if q["query"] in ("zzzzunfindable999", "fermenttaion"):
                    passed = not example["matches"]
                else:
                    flags = 0 if any(c.isupper() for c in needle) else re.IGNORECASE
                    passed = any(re.search(needle, m["text"], flags) for m in example["matches"])
                passed = passed and example["regex_error"] is None
            checks.append({"kind": kind, "query": q["query"], "passed": passed})
    return summary, checks


def process_resources(pid):
    # Darwin sys/resource.h rusage_info_v2; CPU times are Mach ticks, sizes bytes.
    names = ["user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins",
             "wired_size", "resident_size", "phys_footprint", "proc_start_abstime", "proc_exit_abstime",
             "child_user_time", "child_system_time", "child_pkg_idle_wkups", "child_interrupt_wkups",
             "child_pageins", "child_elapsed_abstime", "diskio_bytesread", "diskio_byteswritten"]
    class RusageInfoV2(ctypes.Structure):
        _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(n, ctypes.c_uint64) for n in names]
    info = RusageInfoV2()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    libproc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    if libproc.proc_pid_rusage(pid, 2, ctypes.byref(info)) != 0:
        raise OSError(ctypes.get_errno(), "fff-check driver: proc_pid_rusage failed")
    class MachTimebaseInfo(ctypes.Structure):
        _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]
    timebase = MachTimebaseInfo()
    system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
    if system.mach_timebase_info(ctypes.byref(timebase)) != 0:
        raise RuntimeError("fff-check driver: mach_timebase_info failed")
    return {**{n: getattr(info, n) for n in names}, "ns_per_tick": timebase.numer / timebase.denom}


def capture_idle(pid, output, label, skip):
    if skip:
        return None
    time.sleep(3)  # Outside the measured interval; let event work settle.
    before = process_resources(pid)
    started = time.perf_counter()
    captures = [capture_command(output / f"{label}-top-30s.txt",
                ["top", "-l", "2", "-s", "30", "-stats", "pid,cpu,idlew", "-pid", str(pid)])]
    seconds = time.perf_counter() - started
    after = process_resources(pid)
    # Profilers run AFTER rusage's idle interval, never alongside query timing.
    captures.append(capture_command(output / f"{label}-threads.txt", ["ps", "-M", "-p", str(pid)]))
    captures.append(capture_command(output / f"{label}-sample.txt", ["sample", str(pid), "3", "10", "-file", str(output / f"{label}-stacks.txt")]))
    return {"seconds": seconds, "before": before, "after": after, "captures": captures,
            "cpu_ms": (after["user_time"] + after["system_time"] - before["user_time"] - before["system_time"]) * after["ns_per_tick"] / 1e6,
            "package_idle_wakeups": after["pkg_idle_wkups"] - before["pkg_idle_wkups"],
            "interrupt_wakeups": after["interrupt_wkups"] - before["interrupt_wkups"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/fff-check"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=50)
    parser.add_argument("--watch-trials", type=int, default=5)
    parser.add_argument("--skip-top", action="store_true")
    args = parser.parse_args()
    if args.rounds < 1 or not 1 <= args.watch_trials <= 20:
        parser.error("rounds must be positive and watch-trials must be between 1 and 20")
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="notes-fff-check-") as temp:
        root = Path(temp).resolve()
        vault = root / "vault"
        corpus = generate_vault(vault)
        (args.output / "corpus.json").write_text(json.dumps(corpus, indent=2) + "\n")
        print(json.dumps({"event": "generated", **corpus}), flush=True)
        with (args.output / "time.txt").open("w") as resource_log:
            exec_start = time.perf_counter()
            process = subprocess.Popen(["/usr/bin/time", "-l", str(args.binary.resolve()), str(vault), str(root / "state")],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=resource_log, text=True, bufsize=1,
                                       start_new_session=True)
            try:
                ready = read_reply(process)
                ready["exec_to_ready_ms"] = (time.perf_counter() - exec_start) * 1000
                assert ready["files"] == 10_000, ready
                print(json.dumps(ready), flush=True)
                bench = request(process, {"op": "bench", "rounds": args.rounds, "queries": query_cases()})
                summary, checks = summarize_bench(bench)
                # Store one compact exemplar per query; retain every raw timing sample.
                for row in bench["queries"]:
                    if "paths" in row["example"]:
                        row["example"]["returned_paths"] = len(row["example"]["paths"])
                        row["example"]["paths"] = row["example"]["paths"][:3]
                    if "matches" in row["example"]:
                        row["example"]["returned_matches"] = len(row["example"]["matches"])
                        row["example"]["matches"] = row["example"]["matches"][:3]
                encoded = json.dumps(bench, indent=2)
                encoded = re.sub(r'("samples_ms": )(\[[^]]*\])',
                                 lambda match: match[1] + json.dumps(json.loads(match[2])), encoded)
                (args.output / "queries.json").write_text(encoded + "\n")
                print(json.dumps({"event": "queries", **summary, "failed": [c for c in checks if not c["passed"]]}), flush=True)
                # Supplemental run exposes the opt-in deadline's completeness tradeoff.
                enforced_cases = query_cases()
                for case in enforced_cases:
                    case["enforce_budget"] = True
                enforced = request(process, {"op": "bench", "rounds": args.rounds, "queries": enforced_cases})
                (args.output / "enforced-queries.json").write_text(json.dumps(enforced, indent=2) + "\n")
                enforced_summary, enforced_checks = summarize_bench(enforced)
                explicit = request(process, {"op": "query", "kind": "search", "query": "fermenttaion", "fuzzy": True, "enforce_budget": True})
                (args.output / "explicit-fuzzy.json").write_text(json.dumps(explicit, indent=2) + "\n")
                watcher = check_watcher(process, vault, args.watch_trials, corpus)
                print(json.dumps({"event": "watcher", "observations": watcher}), flush=True)
                idle_active = capture_idle(ready["pid"], args.output, "watcher-active", args.skip_top)
                assert request(process, {"op": "stop_watcher"})["watcher_stopped"]
                manual = check_watcher(process, vault, args.watch_trials, corpus, manual=True)
                print(json.dumps({"event": "manual_updates", "observations": manual}), flush=True)
                idle_control = capture_idle(ready["pid"], args.output, "fff-only", args.skip_top)
                result = {"ready": ready, "summary": summary, "correctness": checks, "watcher": watcher,
                          "manual_updates": manual, "idle_active": idle_active, "idle_control": idle_control,
                          "enforced_summary": enforced_summary, "enforced_correctness": enforced_checks, "rounds": args.rounds, "fff_search": "0.11.0"}
                (args.output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
                process.stdin.write('{"op":"quit"}\n')
                process.stdin.flush()
                if process.wait(timeout=15) != 0:
                    raise RuntimeError("fff-check driver: resident process failed; see time.txt")
                print(json.dumps({"event": "complete", "output": str(args.output)}), flush=True)
                if any(not c["passed"] for c in checks) or any(not w["passed"] for w in watcher + manual):
                    raise RuntimeError("fff-check driver: correctness check failed; see summary.json")
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == "__main__":
    main()
