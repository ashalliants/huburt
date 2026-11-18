#!/usr/bin/env python3
"""Memory footprint of an app plus the WebKit helper processes it owns.

WebKit helpers are launched through XPC (parent pid 1), so they can't be attributed by process tree. Instead:
  measure.py snapshot            -> prints the pids of all WebKit helpers running right now (do this BEFORE launching)
  measure.py total <app_pid> <snapshot-file>   -> footprint of the app + every WebKit helper not in the snapshot
Footprint is macOS's physical footprint (what Activity Monitor calls "Memory"), summed in MB.
"""
import json, re, subprocess, sys

def ps():
    out = subprocess.run(["ps", "-axo", "pid=,comm="], capture_output=True, text=True).stdout
    return [(int(l.split(None, 1)[0]), l.split(None, 1)[1].strip()) for l in out.splitlines() if l.strip()]

def webkit_pids():
    return sorted(p for p, c in ps() if "com.apple.WebKit." in c)

def footprint_mb(pid):
    out = subprocess.run(["footprint", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"Footprint:\s+([\d.]+)\s+(KB|MB|GB)", out)
    if not m: return 0.0
    v, u = float(m.group(1)), m.group(2)
    return v / 1024 if u == "KB" else v * 1024 if u == "GB" else v

if __name__ == "__main__":
    if sys.argv[1] == "snapshot":
        print(json.dumps(webkit_pids()))
    else:
        app = int(sys.argv[2]); before = set(json.load(open(sys.argv[3])))
        mine = [p for p in webkit_pids() if p not in before]
        names = dict(ps())
        rows = [("app", app, footprint_mb(app))] + [(names[p].rsplit(".", 1)[-1].replace("xpc/Contents/MacOS/", "")[-30:], p, footprint_mb(p)) for p in mine]
        for n, p, mb in rows: print(f"  {n:32s} {p:>7d} {mb:8.1f} MB")
        print(f"TOTAL {sum(r[2] for r in rows):.1f} MB across {len(rows)} processes")
