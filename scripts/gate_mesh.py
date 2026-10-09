"""Build/test gate for mesh-edit work: exits 0 only when every step passes.

    py scripts/gate_mesh.py [--fast]

--fast skips the web unit tests (type check still runs).
"""

import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WEB = REPO / "web"
NPM = "npm.cmd" if sys.platform == "win32" else "npm"
ENV = dict(os.environ)
ENV.setdefault("CARGO_BUILD_JOBS", "3")
ENV.setdefault("RUST_TEST_THREADS", "3")
if os.environ.get("R5ED_GATE_TMP"):
    ENV["TMP"] = ENV["TEMP"] = ENV["TMPDIR"] = os.environ["R5ED_GATE_TMP"]

STEPS = [
    ("rust tests", ["cargo", "test", "-q", "-p", "rdoc", "-p", "rcomp", "-p", "r5ed-compile", "-p", "r5ed-host"], REPO),
    ("wasm check", ["cargo", "check", "-q", "-p", "r5ed-wasm", "--target", "wasm32-unknown-unknown"], REPO),
    ("clippy", ["cargo", "clippy", "-q", "-p", "rdoc", "-p", "r5ed-wasm", "--", "-D", "warnings"], REPO),
    ("web typecheck", [NPM, "run", "-s", "check"], WEB),
    ("web tests", [NPM, "test", "--silent"], WEB),
]


def main() -> int:
    fast = "--fast" in sys.argv
    if not (WEB / "node_modules").is_dir():
        subprocess.run([NPM, "install", "--no-audit", "--no-fund"], cwd=WEB, check=True)
    for name, cmd, cwd in STEPS:
        if fast and name == "web tests":
            continue
        print(f"[gate] {name}: {' '.join(cmd)}", flush=True)
        r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace", env=ENV)
        if r.returncode != 0 and "Access is denied" in r.stdout + r.stderr:
            # Windows can refuse to spawn a just-linked exe while it is being scanned.
            r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace", env=ENV)
        if r.returncode != 0:
            out = (r.stdout + r.stderr).splitlines()
            print("\n".join(out[-120:]))
            print(f"[gate] FAIL {name} (exit {r.returncode})")
            return 1
    print("[gate] PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
