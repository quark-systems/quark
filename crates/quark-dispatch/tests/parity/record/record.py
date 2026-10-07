"""Record firstmate's dispatch decisions for the parity cases.

Usage: python3 record.py <firstmate checkout>

Runs bin/fm-dispatch-resolve.sh on each case below with stub `curl` and
`quota-axi` (record/bin) and writes ../<case>.json with the inputs and what
firstmate decided. tests/parity.rs holds the native resolver to the same
decisions.
"""
import json, os, re, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
FM = sys.argv[1]
OUT = os.path.dirname(HERE)
os.makedirs(OUT, exist_ok=True)

def row(scope, pct, sp, runway="through_reset", **kw):
    r = {"scope": scope, "status": "known", "effectivePercentRemaining": pct, "runway": {"status": runway}}
    if sp is not None: r["selection"] = {"spendPriority": sp}
    r.update(kw); return r
def prov(name, rows, status="known"):
    return {"provider": name, "quotaSemantics": {"status": status, "effectiveAvailability": rows}}
def snap(*ps): return {"schemaVersion": 5, "providers": list(ps)}
def ans(rules, choice, conf):
    keys = [f"rule_{i+1}" for i in range(len(rules["rules"]))] + ["default"]
    rest = (1 - 0.9) / (len(keys) - 1)
    probs = {k: (0.9 if k == choice else rest) for k in keys}
    if choice not in keys: probs = {k: 1/len(keys) for k in keys}
    return {"model": "jev-1", "answers": {"rule": {"choice": choice, "confidence": conf, "probabilities": probs}}}

C = {"provider": "system1"}
BAL = {"classifier": C, "rules": [
    {"when": "trivial", "use": [{"harness": "claude", "model": "claude-sonnet-5"}, {"harness": "codex", "model": "gpt-5.5"}]},
    {"when": "risky", "approval": "captain", "use": {"harness": "claude"}},
    {"when": "ordered", "select": "ordered", "use": [{"harness": "codex"}, {"harness": "claude", "effort": "high"}]}],
  "default": [{"harness": "claude", "effort": "medium"}]}
FB = dict(BAL, classifier={"provider": "system1", "on_failure": "default"})
FLOORS = {"classifier": C, "rules": [
    {"when": "x", "floor": {"provider": "claude", "scope": "all_models", "min_percent": 80}, "use": {"harness": "codex"}},
    {"when": "y", "use": {"harness": "claude", "floor": {"scope": "all_models", "min_percent": 90}}}],
  "default": {"harness": "claude", "effort": "low"}}
BUDGET = {"classifier": C, "rules": [{"when": "x", "use": [{"harness": "claude"}, {"harness": "pi", "pricing": "budget"}]}]}
two = lambda a, b: snap(prov("claude", [row("all_models", 70, a)]), prov("codex", [row("all_models", 40, b)]))

S = {
 "balanced": (BAL, ans(BAL, "rule_1", 0.9), two(0.2, 0.7)),
 "tie": (BAL, ans(BAL, "rule_1", 0.9), two(0.5, 0.5)),
 "approval": (BAL, ans(BAL, "rule_2", 0.9), two(0.2, 0.7)),
 "ordered": (BAL, ans(BAL, "rule_3", 0.9), two(0.2, 0.7)),
 "ordered_unverifiable": (BAL, ans(BAL, "rule_3", 0.9), snap(prov("claude", [row("all_models", 70, 0.4)]))),
 "ordered_no_spend_priority": (BAL, ans(BAL, "rule_3", 0.9), snap(prov("claude", [row("all_models", 70, 0.4)]), prov("codex", [row("all_models", 40, None)]))),
 "balanced_no_spend_priority": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 70, 0.4)]), prov("codex", [row("all_models", 40, None)]))),
 "ambiguous": (BAL, ans(BAL, "rule_1", 0.3), two(0.2, 0.7)),
 "fallback_low_confidence": (FB, ans(FB, "rule_1", 0.3), two(0.2, 0.7)),
 "fallback_http": (FB, "500", two(0.2, 0.7)),
 "error_http": (BAL, "500", two(0.2, 0.7)),
 "default_choice": (BAL, ans(BAL, "default", 0.9), two(0.2, 0.7)),
 "invalid_choice_bad_response": (BAL, ans(BAL, "rule_9", 0.9), two(0.2, 0.7)),
 "exhausted_and_zero": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 0, 0.9)]), prov("codex", [row("model:gpt-5.5", 30, 0.1, runway="exhausted_now")]))),
 "overage": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 0, 0.9, overage={"allowed": True, "active": True})]), prov("codex", [row("all_models", 40, 0.2)]))),
 "model_scopes": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 70, 0.6), row("model:claude-sonnet-5", 10, 0.1), row("model:claude-opus-5", 90, 0.9)]), prov("codex", [row("all_models", 40, 0.3)]))),
 "unmeasured": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 70, 0.6)], status="partial"), prov("codex", [{"scope": "all_models", "status": "unknown"}], status="unknown"))),
 "unknown_row": (BAL, ans(BAL, "rule_1", 0.9), snap(prov("claude", [row("all_models", 70, 0.6), {"scope": "model:claude-sonnet-5", "status": "unknown"}]), prov("codex", [row("all_models", 40, 0.3)]))),
 "budget_only": (BUDGET, ans(BUDGET, "rule_1", 0.9), snap()),
 "budget_skipped": (BUDGET, ans(BUDGET, "rule_1", 0.9), two(0.3, 0.1)),
 "rule_floor_below": (FLOORS, ans(FLOORS, "rule_1", 0.9), two(0.3, 0.1)),
 "profile_floor_below": (FLOORS, ans(FLOORS, "rule_2", 0.9), two(0.3, 0.1)),
 "rule_floor_unverifiable": (FLOORS, ans(FLOORS, "rule_1", 0.9), snap(prov("codex", [row("all_models", 40, 0.2)]))),
 "invalid_snapshot": (BAL, ans(BAL, "rule_1", 0.9), {"schemaVersion": 4, "providers": []}),
}

def parse(out):
    status=None; profile=None; cands=[]
    for line in out.splitlines():
        k, _, v = line.strip().partition(":")
        v = v.strip()
        if k == "status": status = v
        elif k == "candidate":
            head, _, verdict = v.rpartition("->")
            hm = head.split()[0]; h, _, m = hm.partition(":")
            cands.append({"harness": h, "model": None if m == "-" else m, "eligible": not verdict.strip().startswith("not eligible")})
        elif k == "profile":
            w = [x.strip("'") for x in v.split()]
            d = dict(zip(w[::2], w[1::2]))
            profile = {"harness": d["--harness"], "model": d.get("--model"), "effort": d.get("--effort")}
    return {"status": status, "profile": profile, "candidates": cands}

for name, (rules, resp, quota) in S.items():
    d = tempfile.mkdtemp()
    os.makedirs(f"{d}/config")
    json.dump(rules, open(f"{d}/config/crew-dispatch.json", "w"))
    json.dump(quota, open(f"{d}/quota.json", "w"))
    code = "200"
    if resp == "500": code, resp = "500", {"error": "boom"}
    json.dump(resp, open(f"{d}/resp.json", "w")); open(f"{d}/code", "w").write(code)
    open(f"{d}/brief.md", "w").write("Fix the parser.\n")
    env = dict(os.environ, PATH=os.path.join(HERE, "bin") + ":" + os.environ["PATH"], FM_HOME=d, TYPESAFE_API_KEY="k",
               PARITY_RESPONSE=f"{d}/resp.json", PARITY_CODE=f"{d}/code", PARITY_QUOTA=f"{d}/quota.json")
    p = subprocess.run([f"{FM}/bin/fm-dispatch-resolve.sh", f"{d}/brief.md", "--project", "quark"], env=env, capture_output=True, text=True)
    if p.returncode != 0:
        print(name, "EXIT", p.returncode, p.stderr); continue
    fixture = {"rules": rules, "classifier_response": resp, "http_code": int(code), "quota": quota, "firstmate": parse(p.stdout), "firstmate_output": re.sub(r"latency_ms: \d+", "latency_ms: 0", p.stdout)}
    json.dump(fixture, open(f"{OUT}/{name}.json", "w"), indent=1)
    print(name, fixture["firstmate"]["status"], fixture["firstmate"]["profile"])
