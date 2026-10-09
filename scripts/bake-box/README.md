# Linux bake box: what it is, what to upload, how to run it

Windows keeps the whole ship except the one CPU-hot step. This box does ONLY
`r5ed-compile build --bake final` (ray-traced lightmaps, scales with
`RAYON_NUM_THREADS`) and hands `project/build` back for package/ship/mod.

This script and README live in the repo (`scripts/bake-box/`), so the box
pulls them with everything else -- no bundle needed.

## Why this split works (verified against r5ed source)

- The Rust core is portable: no `cfg(windows)`, no winapi in any bake-path
  crate, so `cargo build --release -p r5ed-compile` compiles on Linux.
- `build --write-assets` serializes the exact table the Windows `--install`
  build compiled against (stock bounds, tool materials, surface props). The
  box build takes that file with `--assets`, so both sides run identical code
  on identical tables and the outputs match by construction.
- With `--assets`, `--install` only adds the default sky
  (`mdl/vistas/eden_se.rmdl` when the project names none) and never opens a
  game root: no paks, no index, no RSX on the box.
- The bake never writes outside `jobs/<stem>/` (plus cargo target/).

## Box spec

Windows is NOT needed here. Order: 6-8 vCPUs (bake scales to
`RAYON_NUM_THREADS`, default `nproc`), 16 GB RAM, 100 GB SSD. No GPU
(`rbake` is CPU ray tracing). Any distro with curl + gcc (rustup needs a
linker) works.

## Upload list (one time)

The tables path below is the way. On the box, clone the same revision the
Windows side builds with:

  git clone https://github.com/R5Flowstate/r5ed.git ~/r5ed-bake/r5ed-src
  cd ~/r5ed-bake/r5ed-src && git checkout <the Windows side's revision>

then per map, copy `tables.json` (written by `--write-assets`, see below)
plus `<project>/{map.json,meshes/}` to `$BOX/jobs/<stem>/`. No game paks, no
`assets.json`, no RSX on the box.

Legacy full-install route (only if a map ever needs something the table path
cannot give): `rsync -a` the 22 GB of `paks/Win64/*.rpak` to
`$BOX/game/paks/Win64/` with mtimes preserved, plus a fresh `assets.json` to
`r5ed-src/target/release/r5ed-cache/index/`. This was the original design and
is superseded: the copied index reads as stale when its stored `game_root`
differs, and `Install::material/model` export through RSX at build time, so a
stub cannot satisfy it.


## Lighter path: one asset table instead of 22 GB of paks

The pak route above has two gaps on a Linux box. `AssetIndex::refresh` drops
the copied index when its `game_root` string (the Windows game folder) differs from the
box's root, so every pak reads as stale and RSX is asked to relist them. Even
with a fresh index, `Install::material` and `Install::model` export each
material and the sky vista through RSX into `r5ed-cache/rsx/`, and the stub
fails those too.

`build --write-assets <file>` writes the asset table a build compiled
against. Built with `--assets <file> --install`, a map gets that table and the
default sky and never opens a game root, so the box needs no paks, no index
and no RSX:

On Windows, after "src paks" (seconds, no bake):

  r5ed-compile build <project> --install --bake none --write-assets tables.json

Copy `tables.json` and `<project>/{map.json,meshes/}` to the box, then:

  r5ed-compile build <project> --assets tables.json --install --bake final

and copy `<project>/build` back; the ship continues at "package". The table
is only valid for the document it was written from, so write it again after
an edit that adds a material or model.

## Handing the bake back without baking it twice

Every build writes `build/build.json`: a digest of its inputs and its
report. The digest covers the compiler version, the bake mode and targets,
the document (re-serialized, so a `json.dump` of map.json does not count),
every mesh/terrain file it names, and the asset table entries the document
resolves. A `build` whose output dir already holds a record of the same
inputs, with every file it lists, prints `REUSED ...` and returns in
seconds. `--rebuild` forces a compile.

The box's `build/` ships as is. Set `PREBUILT=<that build dir>` for
`ship_common.sh`, or put it at `<out dir>/project/build` for
`scripts/apexm/ship_level.sh`, which sets `PREBUILT` by itself. The ship then
runs **no build at all**: it copies the bake in and goes straight to package.
The map ships exactly as the box baked it. An earlier version only reused the
bake when the input digest matched. A materials step that regenerated the
table broke the match, and the map silently baked again.

Before this, the record did not exist, and a ship always re-baked what the box
had already baked: about 30 minutes for CyberPark.

## Full table + map table, and the unresolved check

A table only carries what the install resolved, and the install never
resolves a map's authored materials (`world/obj/<stem>/...`): they live in
its `<stem>_src` pak, and RSX cannot read a pak repak built (it exits
-1073740791 on it, so the index gets no rows for it however it is named).
`--src-pak <stage>/src_pak/<stem>_src.json` reads them instead from the repak
manifest and the material json objmat/srcmat wrote beside it, with our own
parser and guid hash, no tool involved:

  r5ed-compile build <project> --install --bake none --src-pak <stage>/src_pak/<stem>_src.json --write-assets tables.json

`--write-assets` from an install also records the models the map packs but
the compiler never reads (entity models, the podium and other basement
rooms' props); the unresolved check leaves those out on purpose, since no
compile reads them.

A full table, written once per game build, covers every stock material
(and, if asked, model), so a map that only uses stock assets needs no table
of its own:

  r5ed-compile build <any project> --install --bake none --write-assets full.json --all-assets materials
  r5ed-compile build <any project> --install --bake none --write-assets full.json --all-assets materials,models

The first run exports every asset through RSX, one pak at a time, printing
`ASSETS done/total <kind> <pak>` per pak; it takes a while, and later runs
reuse the batch exports under `r5ed-cache/batch/`. `models` exports every
model's vertex data too, so it costs far more time and disk than
`materials`; the box only reads models for props and the sky vista. The
report's `assets_written.failed` counts assets the tool could not export.
`full.json` is a generated file: it travels beside the map zips, out of band,
and is never committed.

`--assets` takes several files, comma separated, merged in order with a later
file's entry winning, and finds the document's names in them the way the
install does (exact name, then the `_wldc` world variant):

  r5ed-compile build <project> --assets full.json,tables.json --install --bake final

Every build now reports the names the compiler reads that its table lacks
(`unresolved` in the JSON, a WARNING line on stderr); those compile on
defaults (lit bump, opaque, default surface prop). A `final` bake from
`--assets` refuses to run with any left, so a stale or partial table can no
longer light a map on defaults in silence; `--allow-unresolved` overrides it.
An `--install` build only warns.

## Run it

On Windows (normal ship, stop after "src paks", then write the table --
seconds, no bake):

  r5ed-compile build <project> --install --bake none --src-pak <stage>/src_pak/<stem>_src.json --write-assets tables.json

Copy `tables.json` and `<project>/{map.json,meshes/}` to `$BOX/jobs/<stem>/`.

On the box (from the repo checkout):

  BOX=~/r5ed-bake ~/r5ed-bake/r5ed-src/scripts/bake-box/bake-linux.sh <stem>

Back on Windows: copy `jobs/<stem>/project/build` over the project's `build/`
and continue the ship at "package". Gate, mod, deploy unchanged.

## Freshness rule (the only operational gotcha)

A `tables.json` matches the document it was written from. Add a material or
a model on Windows and the box bake stops on "unresolved asset(s)" --
rewrite the table (`--bake none` takes seconds) and re-copy it with the map.
A `tables.json` never goes stale any other way: no paks, no index, nothing
to refresh. `full.json` goes stale with the game build: rewrite it when the
install's paks change. Never "fix" a table miss by installing Wine.

## Knobs

- `RAYON_NUM_THREADS=N` (default: all cores). Bake time scales ~linearly to
  ~8-12 cores for surf-size maps, then flattens on memory bandwidth.
- `map.json` bake settings per map: `texel_size` (16), `samples` (256),
  `bounces` (2). Halving samples roughly halves bake time for a noisier first
  look; ship `final` for anything the user will see.
- `BOX=~/r5ed-bake` elsewhere: `BOX=/mnt/fastdisk/r5ed-bake ./bake-linux.sh x`.
