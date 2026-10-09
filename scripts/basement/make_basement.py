"""Write the stock-room file (R5ED_BASEMENT) from the user's own install.

    py scripts/basement/make_basement.py [out json]

Imports mp_rr_divided_moon_mu1 with r5ed-compile into scratch, then lifts its
character select, ceremony, scene capture and podium rooms (extract_basement.py).
Run once per install; builds read the file R5ED_BASEMENT names.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "lib"))
import r5edenv  # noqa: E402

SOURCE_MAP = "mp_rr_divided_moon_mu1"


def compile_exe() -> Path:
    bin_dir = Path(r5edenv.get("R5ED_BIN", required=False) or HERE.parents[1] / "target" / "release")
    for name in ("r5ed-compile.exe", "r5ed-compile"):
        if (bin_dir / name).is_file():
            return bin_dir / name
    raise SystemExit(f"r5ed-compile not found in {bin_dir}: build it (cargo build --release -p r5ed-compile) or set R5ED_BIN")


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else r5edenv.path("R5ED_BASEMENT")
    r5edenv.export_all()
    work = r5edenv.scratch("basement")
    project = work / SOURCE_MAP
    r = subprocess.run([str(compile_exe()), "import-map", SOURCE_MAP, str(project), "--force",
                        "--cache", str(work / "cache")], capture_output=True, text=True)
    if r.returncode != 0 or not (project / "map.json").is_file():
        sys.stdout.write(r.stdout[-2000:] + r.stderr[-2000:])
        raise SystemExit(f"importing {SOURCE_MAP} failed")
    out.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run([sys.executable, str(HERE / "extract_basement.py"), str(project / "map.json"), str(out)])
    if r.returncode != 0 or not out.is_file():
        raise SystemExit("extracting the rooms failed")
    print(f"wrote {out}; set R5ED_BASEMENT to it")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
