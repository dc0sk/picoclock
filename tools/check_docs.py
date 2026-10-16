#!/usr/bin/env python3
"""Validates the header block of every markdown file under docs/.

Rules: line 1 is `---`; exactly the fields project, doc, status, last_updated; project is the repo slug; doc equals
the file's own path; status is one of living / design / ledger; last_updated is an ISO date; and `living` is allowed
only for the documents a generator writes (GENERATED below, kept in step with tools/trace.py).
`self-test` feeds known-good and known-bad headers to the validator.
"""
import datetime, glob, os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECT = "picoclock"
STATUSES = {"living", "design", "ledger"}
GENERATED = {"docs/srs.md", "docs/traceability-matrix.md"}
FIELDS = ["project", "doc", "status", "last_updated"]


def problems(path, text):
    lines = text.split("\n")
    if lines[0] != "---":
        return ["line 1 is not `---`"]
    try:
        end = lines.index("---", 1)
    except ValueError:
        return ["header block is not closed with `---`"]
    kv = {}
    for l in lines[1:end]:
        m = re.match(r"([a-z_]+): (.*)$", l)
        if not m:
            return [f"malformed header line {l!r}"]
        kv[m.group(1)] = m.group(2)
    out = []
    if list(kv) != FIELDS:
        out.append(f"fields are {list(kv)}, expected {FIELDS}")
        return out
    if kv["project"] != PROJECT:
        out.append(f"project is {kv['project']!r}")
    if kv["doc"] != path:
        out.append(f"doc is {kv['doc']!r}, the file is {path!r}")
    if kv["status"] not in STATUSES:
        out.append(f"status {kv['status']!r} not in {sorted(STATUSES)}")
    if kv["status"] == "living" and path not in GENERATED:
        out.append("status living, but no generator writes this file")
    if kv["status"] != "living" and path in GENERATED:
        out.append("a generated file must be status living")
    try:
        datetime.date.fromisoformat(kv["last_updated"])
    except ValueError:
        out.append(f"last_updated {kv['last_updated']!r} is not an ISO date")
    return out


def self_test():
    good = "---\nproject: picoclock\ndoc: docs/x.md\nstatus: design\nlast_updated: 2026-10-16\n---\nbody"
    cases = {
        "good": (good, []),
        "wrong doc": (good.replace("docs/x.md", "docs/y.md"), ["doc"]),
        "bad date": (good.replace("2026-10-16", "16.10.2026"), ["ISO"]),
        "bad status": (good.replace("design", "wip"), ["status"]),
        "living without generator": (good.replace("design", "living"), ["generator"]),
        "no header": ("# title", ["line 1"]),
        "extra field": (good.replace("status: design", "status: design\nowner: me"), ["fields"]),
    }
    failed = 0
    for name, (text, want) in cases.items():
        got = problems("docs/x.md", text)
        ok = (not got) if not want else all(any(w in g for g in got) for w in want)
        print(f"self-test {'ok' if ok else 'FAILED'}: {name}")
        failed += not ok
    return 1 if failed else 0


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "self-test":
        return self_test()
    files = sorted(glob.glob(os.path.join(ROOT, "docs", "**", "*.md"), recursive=True))
    if not files:
        print("no markdown files found under docs/ (a scan that finds nothing is a failure)")
        return 1
    bad = 0
    for f in files:
        rel = os.path.relpath(f, ROOT)
        for p in problems(rel, open(f, encoding="utf-8").read()):
            print(f"{rel}: {p}")
            bad += 1
    print(f"{len(files)} docs checked: {'FAIL (' + str(bad) + ')' if bad else 'ok'}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
