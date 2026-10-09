"""Build the browser core and publish it under web/public/pkg.

    py scripts/build_wasm.py [--debug] [--out DIR] [--skip-types]

One Rust core serves the browser, the host and the CLI, so this is a plain
cargo build for the wasm target followed by wasm-bindgen. The target features
are probed rather than assumed: a toolchain that cannot take the thread set is
built without it instead of failing the whole run.
"""
import argparse
import gzip
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATE = "r5ed-wasm"
TARGET = "wasm32-unknown-unknown"
OUT = Path("web") / "public" / "pkg"
BUDGET = 3 * 1024 * 1024

# The set the compiler's own crates turn on, and the extra ones only a
# shared-memory build can use. Whatever the toolchain does not list is dropped
# before the flags are written, so a target without threads still builds.
CORE_FEATURES = ["bulk-memory", "mutable-globals", "nontrapping-fptoint", "simd128", "sign-ext"]
THREAD_FEATURES = ["atomics", "shared-memory", "reference-types", "multivalue", "tail-call"]


def log(message):
    print("build-wasm: " + message, flush=True)


def fail(message):
    print("build-wasm: " + message, file=sys.stderr, flush=True)
    return 1


def run(command, env):
    log("$ " + " ".join(str(c) for c in command))
    done = subprocess.run(command, cwd=str(REPO), env=env)
    if done.returncode:
        raise SystemExit(done.returncode)


def tool(name, *extra):
    found = shutil.which(name)
    if not found:
        raise SystemExit(fail("%s is not on the search path" % name))
    return [found, *extra]


def crate_version():
    text = (REPO / "crates" / CRATE / "Cargo.toml").read_text(encoding="utf-8")
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("wasm-bindgen") and "=" in line:
            value = line.split("=", 1)[1].strip().strip('"').lstrip("=")
            if value:
                return value
    return ""


def tool_version(exe):
    done = subprocess.run([exe, "--version"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return done.stdout.strip().rsplit(" ", 1)[-1]


def supported_features():
    """The target features this toolchain actually accepts for the target."""
    rustc = tool(os.environ.get("RUSTC", "rustc"))[0]
    done = subprocess.run(
        [rustc, "--print", "target-features", "--target", TARGET],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if done.returncode:
        return []
    names = set()
    for line in done.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("-"):
            names.add(parts[0])
    return sorted(names)


def feature_set(names, supported):
    keep = [n for n in names if not supported or n in supported]
    return ",".join("+" + n for n in keep) if keep else "+simd128"


def thread_features(supported):
    # Threads need shared memory; without it `atomics` buys nothing and the
    # toolchain only warns that it is unstable.
    if not supported or "shared-memory" in supported:
        return THREAD_FEATURES
    return []


def build_cargo(profile, target_dir, features):
    cargo = tool(os.environ.get("CARGO", "cargo"))
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["RUSTFLAGS"] = (os.environ.get("RUSTFLAGS", "") + " -C target-feature=" + features).strip()
    command = cargo + ["build", "-p", CRATE, "--target", TARGET]
    command.append("--release" if profile == "release" else "--profile=dev")
    run(command, env)


def main(argv):
    parser = argparse.ArgumentParser(description="build the wasm core for the browser")
    parser.add_argument("--debug", action="store_true", help="build the dev profile instead")
    parser.add_argument("--out", default=str(OUT), help="output directory")
    parser.add_argument("--target-dir", default=os.environ.get("CARGO_TARGET_DIR", "target"))
    parser.add_argument("--skip-types", action="store_true", help="do not emit the TypeScript declarations")
    opts = parser.parse_args(argv)

    out = (REPO / opts.out).resolve()
    profile = "dev" if opts.debug else "release"
    target_dir = Path(opts.target_dir)
    if not target_dir.is_absolute():
        target_dir = (REPO / target_dir).resolve()

    wanted = crate_version()
    bindgen = tool("wasm-bindgen")
    have = tool_version(bindgen[0])
    if wanted and have != wanted:
        return fail("the crate asks for wasm-bindgen %s and the tool is %s" % (wanted, have))

    built = None
    supported = supported_features()
    log("the toolchain offers " + (", ".join(supported) if supported else "no target features"))
    for names in (CORE_FEATURES + thread_features(supported), CORE_FEATURES):
        features = feature_set(names, supported)
        log("target features " + features)
        try:
            build_cargo(profile, target_dir, features)
        except SystemExit as failure:
            if failure.code and names is not CORE_FEATURES:
                log("the toolchain refused that feature set")
                continue
            raise
        built = target_dir / TARGET / ("release" if profile == "release" else "debug") / "r5ed_wasm.wasm"
        break
    if built is None or not built.is_file():
        return fail("cargo produced no %s" % built)

    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, exist_ok=True)
    command = bindgen + ["--target", "web", "--out-dir", str(out), str(built)]
    if opts.skip_types:
        command.append("--no-types")
    run(command, dict(os.environ))

    script = out / "r5ed_wasm.js"
    binary = out / "r5ed_wasm_bg.wasm"
    for produced in (script, binary):
        if not produced.is_file():
            return fail("wasm-bindgen wrote no %s" % produced.name)
    raw = binary.stat().st_size
    packed = len(gzip.compress(binary.read_bytes(), 9))
    log("%s is %.1f KB raw, %.1f KB gzip of a %d KB budget"
        % (binary.name, raw / 1024.0, packed / 1024.0, BUDGET // 1024))
    if packed > BUDGET:
        return fail("the wasm core is over the gzip budget")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
