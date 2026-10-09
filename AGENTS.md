# r5ed for agents

r5ed turns a map into an R5Flowstate (Apex Legends S21) map mod: client map, dedicated-server
map, paks, scripts, loading screen, minimap and the mod package, installed into the game.
This file is the entry point for an AI agent driving it. Read it, then the guide for the
source you are porting (table below).

## 1. One-time setup

Tools (all free; build or download each once):

| setting | tool | where |
|---|---|---|
| `R5ED_GAME_ROOT` | the R5Flowstate (S21) game | https://r5flowstate.org -- the folder holding `paks/Win64` |
| `R5ED_RSX` | RSX, reads the game paks | https://github.com/R5Flowstate/rsx |
| `R5ED_REPAK` | RePak, writes paks | https://github.com/R5Flowstate/RePak |
| `R5ED_REVPK` | revpk, writes the server VPK | https://github.com/R5Flowstate/s21-r5sdk (`src/revpk`) |
| `R5ED_RMDLCONV` | rmdlconv, converts models | https://github.com/R5Flowstate/rmdlconv |
| `R5ED_TEXCONV` | texconv | https://github.com/microsoft/DirectXTex/releases |
| `R5ED_PROJECTS` | where projects live | any folder |
| `R5ED_SCRATCH` | exports and staging | any folder with tens of GB free |
| `R5ED_BASEMENT` | the stock-room file (step 4 below) | a path in scratch |
| `R5ED_BLENDER` (optional) | Blender, for .blend/.fbx/.dae sources | https://www.blender.org |
| `R5ED_RECAST` (optional) | the navmesh baker, for AI navigation | the R5Flowstate SDK |

1. Copy `r5ed.config.example.json` to `r5ed.config.json` and fill it in (or set the same names
   as environment variables; `R5ED_CONFIG` can point at a config file elsewhere).
2. Build: `cargo build --release` (Rust stable). The editor UI also needs
   `py scripts/build_wasm.py` and `npm --prefix web install && npm --prefix web run build`;
   the porting pipelines only need the CLI.
3. Python 3.10+ with `numpy`, `Pillow` (`pip install numpy pillow`); Git Bash for the `.sh` scripts.
4. `py scripts/basement/make_basement.py` once: writes the character select, ceremony and
   podium rooms every map needs, read from your own install. Point `R5ED_BASEMENT` at it.
5. Material templates (once): the porting scripts rebuild every foreign material on a stock
   S21 material of the same kind, exported from your install into `$R5ED_SCRATCH/templates`
   (the first ship does this; see the porting guide).

Nothing from the game is in this repository; everything game-side is read from your install.

## 2. Pick the route

| source | status | guide |
|---|---|---|
| Source engine maps: CS:GO / CS:S / Momentum `.bsp`, `.bsp.bz2`, Hammer `.vmf` (surf, bhop, kz, deathmatch) | one command | [docs/porting/source.md](docs/porting/source.md) |
| A model as a map: `.glb`, `.obj` (+ `.mtl`), and through Blender `.blend`, `.fbx`, `.dae` | one command | [docs/porting/source.md](docs/porting/source.md#models-as-maps) |
| Built in the editor or by CLI verbs | `r5ed-compile build` + `package` | README.md |
| Call of Duty MW2, Halo 2/3, Apex Legends Mobile, Titanfall 2 | advanced: scripts included, outside tools needed | [docs/porting/advanced.md](docs/porting/advanced.md) |

## 3. Rules that keep a port working

- **One ship at a time.** A ship rewrites the shared asset index, copies paks into the game and
  closes the game. The ship scripts hold a lock in `$R5ED_SCRATCH`; a second ship waits on it.
  Prepare several maps in parallel if you like; ship them one by one.
- **Never launch the game yourself.** The person you work for tests in game. A ship closes a
  running game, so tell them before you ship.
- **Gates are the definition of done.** A ship ends with `GATE PASSED` and an installed mod, or
  it failed. Never deploy past a failed gate; fix the cause.
- **Credit the map author** in the mod blurb (`BLURB=`). Ask for the name if the file does not
  say it. Port only maps whose authors allow it.
- **Use your own art, or the map's.** Never ship textures or models taken from the game.
- **Report gaps honestly**: missing materials (`missing_vmt`), frozen movers, unresolved
  teleports, skipped displacements, and that the map is untested until someone plays it.
- Keep scratch on a drive with room; a ship exports and stages several GB.

## 4. When something fails

Each guide has a symptom -> cause -> fix table. Read the ship log's `==` step lines and the
first `FAILED` / `GATE` line; the step names match the guide.
