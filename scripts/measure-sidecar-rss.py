"""L1 — does the brain sidecar's RSS grow monotonically across a run of Ask requests?

RESULT (2026-09-04, Qwen3-1.7B-Q4_K_M, aarch64 release build): YES in shape, NO in weight.
Every one of 30 samples was >= the previous, exactly as an undrained autoreleasepool predicts —
but the drift is ~0.66 MB per request (4186 MB after the first generation, 4207 MB after the
30th). The resident model FLOOR is ~4.2 GB; the leak above it costs tens of MB in a normal
session. See docs/research/2026-09-02-full-app-analysis.md, item L1.

Drives the REAL `murmur-brain` binary over its shipped NDJSON stdin protocol (the same one
`reason/sidecar.rs` speaks) and samples RSS after every generation. The audit's recipe is
`ps -o rss= -p $(pgrep ...)` across 1 -> 10 -> 30 requests; this does the 30 in one process and
records the whole curve, which is strictly more informative than three spot readings.
"""
import json, subprocess, sys, time, os

BIN   = os.environ.get(
    "MURMUR_BRAIN_BIN",
    "target/aarch64-apple-darwin/release/murmur-brain",
)
MODEL = os.environ.get("MURMUR_BRAIN_MODEL", "")  # absolute path to a .gguf; required
N     = int(os.environ.get("MURMUR_BRAIN_REQUESTS", "30"))

def rss_kb(pid):
    out = subprocess.run(["ps","-o","rss=","-p",str(pid)],capture_output=True,text=True)
    try: return int(out.stdout.strip())
    except ValueError: return -1

if not MODEL:
    print("set MURMUR_BRAIN_MODEL to an absolute .gguf path (see the module docstring)")
    sys.exit(2)

p = subprocess.Popen([BIN,"--model",MODEL,"--max-idle-seconds","900"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                     text=True, bufsize=1)

def send(obj):
    p.stdin.write(json.dumps(obj)+"\n"); p.stdin.flush()

def wait_for(kinds, timeout=600):
    end = time.time()+timeout
    while time.time() < end:
        line = p.stdout.readline()
        if not line:
            return None
        try: m = json.loads(line)
        except Exception: continue
        if m.get("type") in kinds: return m
    return None

t0=time.time()
send({"type":"ready_probe"})
ready = wait_for({"ready"}, timeout=900)
if ready is None:
    print("SIDECAR NEVER BECAME READY"); p.kill(); sys.exit(1)
load_s = time.time()-t0
base = rss_kb(p.pid)
print(f"loaded in {load_s:.1f}s   RSS after load: {base/1024:.0f} MB   pid={p.pid}")
print("req  rss_mb  delta_mb  gen_s")

samples=[]
for i in range(1, N+1):
    t=time.time()
    send({"type":"generate","id":i,
          "system":"You answer in one short sentence.",
          "user":f"Summarise in one sentence why meeting number {i} mattered.",
          "opts":{"max_tokens":48,"temperature":0.7,"enable_thinking":False,
                  "use_grammar_constraint":False}})
    msg = wait_for({"done","error"})
    dt=time.time()-t
    if msg is None:
        print(f"{i:3d}  NO REPLY"); break
    if msg.get("type")=="error":
        print(f"{i:3d}  ERROR {msg.get('kind')} {msg.get('message')[:60]}"); break
    r = rss_kb(p.pid); samples.append(r)
    print(f"{i:3d}  {r/1024:7.0f}  {(r-base)/1024:+8.1f}  {dt:5.1f}")

if samples:
    print(f"\nbaseline {base/1024:.0f} MB -> final {samples[-1]/1024:.0f} MB "
          f"(delta {(samples[-1]-base)/1024:+.1f} MB over {len(samples)} requests)")
    first10 = samples[9] if len(samples)>=10 else samples[-1]
    print(f"after 10: {first10/1024:.0f} MB   after {len(samples)}: {samples[-1]/1024:.0f} MB "
          f"  growth 10->{len(samples)}: {(samples[-1]-first10)/1024:+.1f} MB")
send({"type":"shutdown"})
try: p.wait(timeout=30)
except Exception: p.kill()
