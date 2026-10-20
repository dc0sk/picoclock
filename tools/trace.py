#!/usr/bin/env python3
"""Requirements traceability: renders docs/srs.md and docs/traceability-matrix.md from docs/requirements.toml and
checks that the chain requirement -> capability -> code -> test/record holds.

  tools/trace.py check [--tests-output FILE]   fail on GAP / ORPHAN / DANGLING / ... (see CHECKS below)
  tools/trace.py render                        regenerate the two documents
  tools/trace.py self-test                     plant each violation and confirm the checker goes red

Bindings are read from the code, not typed into the documents:
  * unit tests: a `// VERIFIES: REQ-..` comment directly above a `#[test]` in a Rust file under core/ or firmware/
  * records   : `Verifies:` lines under `### VR-nn · date · tier · title` headings in docs/verification-log.md
With --tests-output (the saved output of `cargo test`) every bound test must also appear there as `... ok`.
"""
import copy, datetime, fnmatch, glob, hashlib, os, re, sys, tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REQS = "docs/requirements.toml"
LOG = "docs/verification-log.md"
SRS = "docs/srs.md"
MATRIX = "docs/traceability-matrix.md"
PROJECT = "picoclock"
SOURCE_GLOBS = ["core/src/**/*.rs", "firmware/src/**/*.rs", "firmware/build.rs", "bootloader/src/**/*.rs", "bootloader/build.rs"]
TEST_GLOBS = ["core/src/**/*.rs", "core/tests/**/*.rs", "firmware/src/**/*.rs"]
MIN_EVIDENCE = {"high": 2, "medium": 1, "low": 1}
STATUSES = {"draft", "ratified", "retired"}
TIER_ORDER = ["unit", "host", "build", "hardware"]


def rel(p):
    return os.path.relpath(p, ROOT)


def files_matching(globs):
    out = set()
    for g in globs:
        out.update(rel(p) for p in glob.glob(os.path.join(ROOT, g), recursive=True) if os.path.isfile(p))
    return sorted(out)


def module_of(path):
    """core/src/cli.rs -> cli ; core/src/lib.rs -> '' (tests in lib are crate-level) ; core/tests/x.rs -> '' (an
    integration test binary: cargo prints the bare test name)."""
    stem = os.path.splitext(os.path.basename(path))[0]
    return "" if stem == "lib" or path.startswith("core/tests/") else stem


def load():
    with open(os.path.join(ROOT, REQS), "rb") as f:
        data = tomllib.load(f)
    reqs, caps = data.get("req", []), data.get("cap", [])
    files = files_matching(SOURCE_GLOBS)
    bindings = {}  # req id -> [(file, test name)]
    unbound = []  # (file, line) of a VERIFIES line that is not directly above a #[test] function
    for path in files_matching(TEST_GLOBS):
        lines = open(os.path.join(ROOT, path), encoding="utf-8").read().split("\n")
        for i, line in enumerate(lines):
            m = re.match(r"\s*// VERIFIES: (.+)$", line)
            if not m:
                continue
            j = i + 1
            attrs = []
            # between the binding and its function only comments and attributes may stand, and one attribute must be #[test]
            while j < len(lines) and re.match(r"\s*(//|#\[)", lines[j]):
                attrs.append(lines[j].strip())
                j += 1
            if j >= len(lines) or not re.match(r"\s*(?:async\s+)?fn \w+", lines[j]) or not any(re.match(r"#\[(?:\w+::)?test\b", a) for a in attrs):
                unbound.append((path, i + 1))
                continue
            name = re.match(r"\s*(?:async\s+)?fn (\w+)", lines[j]).group(1)
            for rid in m.group(1).split():
                bindings.setdefault(rid, []).append((path, name))
    records = []
    text = open(os.path.join(ROOT, LOG), encoding="utf-8").read()
    for m in re.finditer(r"^### (VR-\d+) · (\d{4}-\d{2}-\d{2}) · (\w+) · (.+?)\n(.*?)(?=^### |\Z)", text, re.M | re.S):
        v = re.search(r"^Verifies: (.+)$", m.group(5), re.M)
        records.append({"id": m.group(1), "date": m.group(2), "tier": m.group(3), "title": m.group(4),
                        "verifies": [x.strip() for x in v.group(1).split(",")] if v else []})
    return {"reqs": reqs, "caps": caps, "files": files, "bindings": bindings, "records": records, "unbound": unbound}


def evidence(model, rid):
    tests = model["bindings"].get(rid, [])
    recs = [r for r in model["records"] if rid in r["verifies"]]
    tiers = ({"unit"} if tests else set()) | {r["tier"] for r in recs}
    return tests, recs, tiers


def check(model, tests_output=None):
    """Returns a list of (code, message)."""
    bad = []
    reqs = {r["id"]: r for r in model["reqs"]}
    caps = {c["id"]: c for c in model["caps"]}
    if len(reqs) != len(model["reqs"]):
        bad.append(("DUPLICATE", "duplicate requirement id"))
    for r in model["reqs"]:
        if r["status"] not in STATUSES:
            bad.append(("BAD-STATUS", f"{r['id']}: status {r['status']!r}"))
        if r["criticality"] not in MIN_EVIDENCE:
            bad.append(("BAD-CRITICALITY", f"{r['id']}: {r['criticality']!r}"))
        for c in r.get("cap", []):
            if c not in caps:
                bad.append(("DANGLING", f"{r['id']} names unknown capability {c}"))
        if r["status"] == "ratified" and not r.get("cap"):
            bad.append(("NO-CAPABILITY", f"{r['id']} is ratified but no capability implements it"))
        if r["status"] == "retired" and not r.get("retired_because"):
            bad.append(("BAD-RETIREMENT", f"{r['id']} is retired without retired_because"))
    # code <-> capability
    owner = {}
    for c in model["caps"]:
        matched = [f for f in model["files"] if any(fnmatch.fnmatch(f, g) for g in c["code"])]
        if not matched:
            bad.append(("DANGLING", f"{c['id']}: code globs match no file"))
        for f in matched:
            owner.setdefault(f, []).append(c["id"])
        if not any(c["id"] in r.get("cap", []) for r in model["reqs"]):
            bad.append(("ORPHAN-CAP", f"{c['id']} implements no requirement"))
    for f in model["files"]:
        n = len(owner.get(f, []))
        if n == 0:
            bad.append(("ORPHAN", f"{f} belongs to no capability"))
        elif n > 1:
            bad.append(("AMBIGUOUS", f"{f} belongs to several capabilities: {owner[f]}"))
    # evidence
    for path, line in model.get("unbound", []):
        bad.append(("UNBOUND-VERIFIES", f"{path}:{line}: a VERIFIES line that is not directly above a #[test] function proves nothing"))
    for rid, items in model["bindings"].items():
        if rid not in reqs:
            bad.append(("DANGLING", f"test binding names unknown {rid}: {items[0][0]}::{items[0][1]}"))
        elif reqs[rid]["status"] == "retired":
            bad.append(("RETIRED-BUT-BOUND", f"{rid} is retired but {items[0][1]} still cites it"))
    for rec in model["records"]:
        for rid in rec["verifies"]:
            if rid not in reqs:
                bad.append(("DANGLING", f"{rec['id']} verifies unknown {rid}"))
            elif reqs[rid]["status"] == "retired":
                bad.append(("RETIRED-BUT-BOUND", f"{rec['id']} cites retired {rid}"))
        if rec["tier"] not in ("hardware", "host", "build"):
            bad.append(("BAD-TIER", f"{rec['id']}: tier {rec['tier']!r}"))
        if not rec["verifies"]:
            bad.append(("EMPTY-RECORD", f"{rec['id']} has no Verifies line"))
    for r in model["reqs"]:
        if r["status"] != "ratified":
            continue
        tests, recs, _ = evidence(model, r["id"])
        n = len(tests) + len(recs)
        waived = "unverified" in r
        if n == 0 and not waived:
            bad.append(("GAP", f"{r['id']} is ratified with no test, no record and no stated reason (unverified)"))
        elif n > 0 and waived:
            bad.append(("STALE-WAIVER", f"{r['id']} has evidence now; remove its `unverified` text"))
        elif n < MIN_EVIDENCE[r["criticality"]] and not waived:
            bad.append(("UNDER-EVIDENCED", f"{r['id']} ({r['criticality']}) has {n} evidence item(s), needs {MIN_EVIDENCE[r['criticality']]}"))
    if tests_output is not None:
        ok = set(re.findall(r"^test (\S+) \.\.\. ok$", tests_output, re.M))
        for rid, items in model["bindings"].items():
            for path, name in items:
                mod = module_of(path)
                full = name if path.startswith("core/tests/") else (f"{mod}::tests::{name}" if mod else f"tests::{name}")
                if path.startswith("firmware/"):
                    bad.append(("CITED-BUT-DIDNT-RUN", f"{rid}: {path}::{name} is firmware code; host test output cannot show it ran"))
                elif full not in ok:
                    bad.append(("CITED-BUT-DIDNT-RUN", f"{rid}: {full} is not in the test run as ok"))
    return bad


# ------------------------------------------------------------------------------------------------ rendering

def table(headers, rows):
    cells = [headers] + rows
    w = [max(len(str(r[i])) for r in cells) for i in range(len(headers))]
    fmt = lambda r: "| " + " | ".join(str(c).ljust(w[i]) for i, c in enumerate(r)) + " |"
    return "\n".join([fmt(headers), "|" + "|".join("-" * (x + 2) for x in w) + "|"] + [fmt(r) for r in rows])


def evidence_label(model, r):
    if r["status"] != "ratified":
        return "-"
    tests, recs, tiers = evidence(model, r["id"])
    if not tiers:
        return "UNVERIFIED"
    return ", ".join(t for t in TIER_ORDER if t in tiers)


def frontmatter(doc, date):
    return f"---\nproject: {PROJECT}\ndoc: {doc}\nstatus: living\nlast_updated: {date}\n---\n\n"


def stamp(doc, body):
    """Keeps the old last_updated date when the body did not change."""
    path = os.path.join(ROOT, doc)
    date = datetime.date.today().isoformat()
    if os.path.exists(path):
        old = open(path, encoding="utf-8").read()
        m = re.match(r"---\n.*?last_updated: (\S+)\n---\n\n(.*)\Z", old, re.S)
        if m and m.group(2) == body:
            date = m.group(1)
    return frontmatter(doc, date) + body


def render_srs(model):
    reqs = model["reqs"]
    caps = {c["id"]: c for c in model["caps"]}
    out = ["""# Software requirements specification

> Generated from `docs/requirements.toml` by `tools/trace.py render`. Do not edit by hand.

## 1. Purpose and scope

This document specifies the firmware for the Waveshare Pico-Clock-Green fitted with a Raspberry Pi Pico W or
Pico 2 W: an LED matrix clock that keeps UTC in its DS3231 real-time clock, synchronises over WiFi/NTP, shows local
time with daylight saving, and is configured over a USB serial terminal, a web page and three buttons.

Each requirement has a stable ID. A ratified statement is never edited: it is retired with a reason and replaced by a
new ID. Evidence for each requirement is in the [traceability matrix](traceability-matrix.md).

## 2. Definitions

| Term       | Meaning                                                                                  |
|------------|------------------------------------------------------------------------------------------|
| draft      | Proposed or on the roadmap; code may be written against it but it is not yet agreed.     |
| ratified   | Agreed. Needs evidence in proportion to its criticality, or a stated reason why not.     |
| retired    | Withdrawn; kept so history resolves.                                                     |
| criticality| high / medium / low: how much evidence a requirement must carry.                         |
| unverified | A ratified requirement for which no evidence exists yet; the reason is given.            |

## 3. Interfaces and constraints

- Hardware: Pico W (RP2040) or Pico 2 W (RP2350) with CYW43439 radio; DS3231 on I2C1 (0x68); 32x8 LED matrix on
  shift registers; three buttons; buzzer; ambient light sensor on ADC; USB device port.
- Network: WiFi station with DHCP; SNTP client (UDP 123), optional SNTP server (UDP 123), HTTP on port 80, one
  plain-HTTP request to an IP geolocation service when the timezone is `auto`.
- Language and platform: Rust, `no_std`, Embassy; logic that does not touch hardware lives in a host-testable crate.
- The web interface is plain HTTP. Anything on the LAN can read what crosses it.

## 4. Requirements
"""]
    areas = []
    for r in reqs:
        if r["area"] not in areas:
            areas.append(r["area"])
    for n, area in enumerate(areas, 1):
        out.append(f"\n### 4.{n} {area}\n")
        for r in [x for x in reqs if x["area"] == area]:
            caps_txt = ", ".join(caps[c]["name"] for c in r.get("cap", [])) or "none yet"
            out.append(f"**{r['id']}** ({r['status']}, {r['criticality']})  \n{r['statement']}  \n"
                       f"*Why:* {r['rationale']}  \n*Implemented by:* {caps_txt}")
            if "unverified" in r:
                out[-1] += f"  \n*Unverified:* {r['unverified']}"
            if r["status"] == "retired":
                out[-1] += f"  \n*Retired:* {r['retired_because']}"
            out.append("")
    return stamp(SRS, "\n".join(out).rstrip() + "\n")


def render_matrix(model):
    reqs = model["reqs"]
    caps = {c["id"]: c for c in model["caps"]}
    rat = [r for r in reqs if r["status"] == "ratified"]
    lab = {r["id"]: evidence_label(model, r) for r in reqs}
    n_hw = sum(1 for r in rat if "hardware" in lab[r["id"]])
    n_unv = sum(1 for r in rat if lab[r["id"]] == "UNVERIFIED")
    n_unit_only = sum(1 for r in rat if lab[r["id"]] == "unit")
    out = ["""# Traceability matrix

> Generated by `tools/trace.py render` from `docs/requirements.toml`, the `// VERIFIES:` comments in the code and
> `docs/verification-log.md`. `tools/trace.py check` fails the build when this file or the bindings are out of date.
> "Evidence" says how a requirement was checked; read it with the tier in mind: a unit test shows the logic on the
> host, a hardware record shows it ran on the clock, and neither vouches for code changed since.

## Summary
"""]
    out.append(table(["Measure", "Count"], [
        ["Requirements", len(reqs)],
        ["Ratified", len(rat)],
        ["Draft (roadmap)", sum(1 for r in reqs if r["status"] == "draft")],
        ["Retired", sum(1 for r in reqs if r["status"] == "retired")],
        ["Ratified with hardware evidence", n_hw],
        ["Ratified with unit tests only (no hardware, host or build record)", n_unit_only],
        ["Ratified and UNVERIFIED (stated reason)", n_unv],
        ["Unit tests bound to requirements", len({t for v in model["bindings"].values() for t in v})],
        ["Verification records", len(model["records"])],
    ]))
    out.append("\n## Forward: requirement to evidence\n")
    rows = []
    for r in reqs:
        tests, recs, _ = evidence(model, r["id"])
        rows.append([r["id"], r["status"], r["criticality"], ", ".join(c.replace("CAP-", "") for c in r.get("cap", [])) or "-",
                     len(tests), ", ".join(x["id"] for x in recs) or "-", lab[r["id"]]])
    out.append(table(["Requirement", "Status", "Crit.", "Capabilities", "Unit tests", "Records", "Evidence"], rows))
    out.append("\n## Ratified requirements without hardware evidence\n")
    out.append("These have never been seen working on the clock (they may still have unit tests).\n")
    rows = [[r["id"], lab[r["id"]], r.get("unverified", "")] for r in rat if "hardware" not in lab[r["id"]]]
    out.append(table(["Requirement", "Evidence", "Reason given"], rows) if rows else "None.")
    out.append("\n## Backward: capability to code and requirements\n")
    rows = []
    for c in model["caps"]:
        files = [f for f in model["files"] if any(fnmatch.fnmatch(f, g) for g in c["code"])]
        rows.append([c["id"], c["name"], ", ".join(files),
                     ", ".join(r["id"] for r in reqs if c["id"] in r.get("cap", []))])
    out.append(table(["Capability", "Name", "Code", "Requirements"], rows))
    out.append("\n## Unit tests by requirement\n")
    for r in reqs:
        tests = model["bindings"].get(r["id"], [])
        if tests:
            out.append(f"- **{r['id']}**: " + ", ".join(f"`{module_of(p) or 'lib'}::{n}`" for p, n in tests))
    out.append("\n## Verification records\n")
    out.append(table(["Record", "Date", "Tier", "Title", "Verifies"],
                     [[x["id"], x["date"], x["tier"], x["title"], ", ".join(x["verifies"])] for x in model["records"]]))
    return stamp(MATRIX, "\n".join(out).rstrip() + "\n")


def generated_stale(model):
    bad = []
    for doc, fn in ((SRS, render_srs), (MATRIX, render_matrix)):
        path = os.path.join(ROOT, doc)
        if not os.path.exists(path) or open(path, encoding="utf-8").read() != fn(model):
            bad.append(("STALE-GENERATED", f"{doc} differs from what `tools/trace.py render` produces"))
    return bad


# ------------------------------------------------------------------------------------------------ self test

def self_test(model):
    """Each mutation must make check() report the named code; the unmutated model must be clean first."""
    base = check(model)
    if base:
        print("self-test: the real model is not clean:", base)
        return 1
    mutations = {
        "GAP": lambda m: m["reqs"].append({"id": "REQ-X-1", "area": "x", "status": "ratified", "criticality": "low",
                                           "statement": "s", "rationale": "r", "cap": [m["caps"][0]["id"]]}),
        "ORPHAN": lambda m: m["files"].append("core/src/unowned.rs"),
        "AMBIGUOUS": lambda m: m["caps"][1]["code"].append(m["caps"][0]["code"][0]),
        "DANGLING": lambda m: m["bindings"].setdefault("REQ-NOPE-1", [("core/src/x.rs", "t")]),
        "RETIRED-BUT-BOUND": lambda m: m["reqs"][0].update(status="retired", retired_because="test"),
        "UNDER-EVIDENCED": lambda m: [r for r in m["reqs"] if r["id"] == "REQ-CFG-03"][0].update(criticality="high")
        or m["bindings"].pop("REQ-CFG-03"),
        "STALE-WAIVER": lambda m: [r for r in m["reqs"] if r["id"] == "REQ-NTP-01"][0].update(unverified="x"),
        "NO-CAPABILITY": lambda m: [r for r in m["reqs"] if r["id"] == "REQ-NTP-01"][0].update(cap=[]),
        "UNBOUND-VERIFIES": lambda m: m["unbound"].append(("core/src/x.rs", 1)),
        "ORPHAN-CAP": lambda m: m["caps"].append({"id": "CAP-LONELY", "name": "x", "code": ["core/src/lib.rs"]}),
    }
    failed = 0
    for code, mutate in mutations.items():
        m = copy.deepcopy(model)
        mutate(m)
        found = {c for c, _ in check(m)}
        if code not in found:
            print(f"self-test FAILED: planted {code}, checker reported {sorted(found)}")
            failed += 1
        else:
            print(f"self-test ok: {code} detected")
    ran = check(model, tests_output="")
    if not any(c == "CITED-BUT-DIDNT-RUN" for c, _ in ran):
        print("self-test FAILED: empty test run not detected as CITED-BUT-DIDNT-RUN")
        failed += 1
    else:
        print("self-test ok: CITED-BUT-DIDNT-RUN detected")
    return 1 if failed else 0


def main(argv):
    cmd = argv[1] if len(argv) > 1 else "check"
    model = load()
    if cmd == "render":
        for doc, fn in ((SRS, render_srs), (MATRIX, render_matrix)):
            with open(os.path.join(ROOT, doc), "w", encoding="utf-8") as f:
                f.write(fn(model))
            print("wrote", doc)
        return 0
    if cmd == "self-test":
        return self_test(model)
    if cmd == "check":
        out = None
        if "--tests-output" in argv:
            out = open(argv[argv.index("--tests-output") + 1], encoding="utf-8").read()
        bad = check(model, out) + generated_stale(model)
        for code, msg in bad:
            print(f"{code}: {msg}")
        reqs = model["reqs"]
        print(f"{len(reqs)} requirements, {len(model['caps'])} capabilities, {len(model['files'])} source files, "
              f"{sum(len(v) for v in model['bindings'].values())} test bindings, {len(model['records'])} records: "
              f"{'FAIL (' + str(len(bad)) + ')' if bad else 'ok'}")
        return 1 if bad else 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
