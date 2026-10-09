//! Typed wrapper around the RSX CLI, the only tool that can read these paks.
//!
//! Every invocation follows the tool's argument-order rule: all flags first,
//! then all input paths. Nothing here goes through a shell.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::asset_list::{parse_asset_list, AssetRow};
use crate::error::{Error, Result};

/// The flags and pak names of a run, without the directories around them.
fn trace_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| {
            let a = a.to_string_lossy();
            let is_path = a.contains('/') || a.contains('\\');
            if is_path { a.rsplit(['/', '\\']).next().unwrap_or(&a).to_string() } else { a.to_string() }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug)]
pub struct Rsx {
    exe: PathBuf,
}

impl Rsx {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self { exe: exe.into() }
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    fn run(&self, args: &[OsString], cwd: &Path) -> Result<String> {
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-nogui");
        cmd.args(args);
        // A scratch directory that does not exist must not turn into a spawn
        // failure, so the tool runs beside itself when there is nowhere better.
        if cwd.is_dir() {
            cmd.current_dir(cwd);
        } else if let Some(d) = self.exe.parent() {
            cmd.current_dir(d);
        }
        cmd.stdin(std::process::Stdio::null());
        // Two headless runs contend on pak decompression and starpak reads, so
        // exports from concurrent requests take turns.
        static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let started = std::time::Instant::now();
        let out = cmd.output().map_err(|e| Error::RsxSpawn(self.exe.clone(), e))?;
        if std::env::var_os("R5ED_RSX_TRACE").is_some() {
            eprintln!("rsx {:.1}s exit {:?}: {}", started.elapsed().as_secs_f32(), out.status.code(), trace_args(args));
        }
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            let code = out.status.code().unwrap_or(-1);
            return Err(Error::RsxExit { status: code, stderr: text.trim().to_string() });
        }
        Ok(text)
    }

    /// Writes the asset list of `paks` as CSV into `cwd` and parses it.
    pub fn list(&self, paks: &[PathBuf], cwd: &Path) -> Result<Vec<AssetRow>> {
        if paks.is_empty() {
            return Ok(Vec::new());
        }
        std::fs::create_dir_all(cwd).map_err(|e| Error::Io(cwd.to_path_buf(), e))?;
        // One run per pak: RSX has crashed on a whole install that lists fine a
        // pak at a time, and a pak it cannot read must not cost the others.
        let mut rows = Vec::new();
        for (n, pak) in paks.iter().enumerate() {
            let list = cwd.join(format!("assets_{n}.csv"));
            let _ = std::fs::remove_file(&list);
            let args: Vec<OsString> = vec![
                "-nocachedb".into(),
                "--list".into(),
                list.clone().into(),
                "--listformat".into(),
                "csv".into(),
                pak.as_os_str().into(),
            ];
            let read = self.run(&args, cwd).and_then(|_| {
                let text = std::fs::read_to_string(&list).map_err(|_| Error::NoAssetList(list.clone()))?;
                parse_asset_list(&text)
            });
            match read {
                Ok(r) => rows.extend(r),
                Err(e) => eprintln!("warning: skipping {}: {e}", pak.display()),
            }
        }
        Ok(rows)
    }

    /// Exports every asset of `kinds` whose guid is in `guids` out of `paks`
    /// in one run, with `extra` flags; one call stands in for many single
    /// exports, since each run parses the whole pak.
    pub fn export_batch(&self, paks: &[PathBuf], kinds: &[&str], extra: &[&str], guids: &[u64], out_dir: &Path) -> Result<()> {
        if guids.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
        let gf = out_dir.join("export_batch_guids.txt");
        let body: String = guids.iter().map(|g| format!("{g:016x}\n")).collect();
        std::fs::write(&gf, body).map_err(|e| Error::Io(gf.clone(), e))?;
        let mut args: Vec<OsString> = vec!["-nocachedb".into(), "-export".into(), "--exporttypes".into(), kinds.join(",").into()];
        args.extend(extra.iter().map(OsString::from));
        args.extend(["--exportguids".into(), gf.into_os_string(), "--exportdir".into(), out_dir.into()]);
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, out_dir).map(|_| ())
    }

    /// Writes the dependency list of `paks` (RSX adjacency format) to `out`.
    pub fn depfile(&self, paks: &[PathBuf], out: &Path) -> Result<()> {
        let dir = out.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
        let mut args: Vec<OsString> = vec!["-nocachedb".into(), "--depfilepath".into(), out.into()];
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, dir).map(|_| ())
    }

    /// Exports assets of `kinds` whose guid is in `guids` into `out_dir`.
    /// An empty `guids` exports every asset of those kinds.
    pub fn export(&self, paks: &[PathBuf], kinds: &[&str], guids: &[u64], out_dir: &Path) -> Result<()> {
        let mut args: Vec<OsString> = vec!["-nocachedb".into(), "-export".into()];
        if !kinds.is_empty() {
            args.push("--exporttypes".into());
            args.push(kinds.join(",").into());
        }
        if !guids.is_empty() {
            let gf = out_dir.join("export_guids.txt");
            if let Some(p) = out_dir.parent() {
                std::fs::create_dir_all(p).map_err(|e| Error::Io(p.to_path_buf(), e))?;
            }
            std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
            let body: String = guids.iter().map(|g| format!("{g:016x}\n")).collect();
            std::fs::write(&gf, body).map_err(|e| Error::Io(gf.clone(), e))?;
            args.push("--exportguids".into());
            args.push(gf.into());
        }
        args.push("--exportdir".into());
        args.push(out_dir.into());
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, out_dir)?;
        Ok(())
    }

    /// Exports the models with these guids from one pak, each at its full asset
    /// path under `out_dir` (`mdl/<path>.rmdl` beside its `.vg`), so models from
    /// any number of runs share the directory without colliding.
    pub fn export_models(&self, pak: &Path, guids: &[u64], out_dir: &Path) -> Result<()> {
        if guids.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
        let gf = out_dir.join("export_guids.txt");
        let body: String = guids.iter().map(|g| format!("{g:016x}\n")).collect();
        std::fs::write(&gf, body).map_err(|e| Error::Io(gf.clone(), e))?;
        let args: Vec<OsString> = vec![
            "-nocachedb".into(),
            "-export".into(),
            "-exportfullpaths".into(),
            "--exporttypes".into(),
            "mdl_".into(),
            "--exportguids".into(),
            gf.into(),
            "--exportdir".into(),
            out_dir.into(),
            pak.into(),
        ];
        self.run(&args, out_dir).map(|_| ())
    }

    /// Exports the materials with these guids from `paks`, textures named by
    /// guid, each json at its full asset path under `out_dir`.
    pub fn export_materials(&self, paks: &[PathBuf], guids: &[u64], out_dir: &Path) -> Result<()> {
        self.export_guids(paks, "matl", &["--texturenames", "guid"], guids, out_dir)
    }

    /// Exports the textures with these guids from `paks` as DDS, one
    /// `texture/0x<GUID>_level<n>.dds` per mip.
    pub fn export_textures(&self, paks: &[PathBuf], guids: &[u64], out_dir: &Path) -> Result<()> {
        self.export_guids(paks, "txtr", &["--exportsetting", "txtr=3"], guids, out_dir)
    }

    fn export_guids(&self, paks: &[PathBuf], kind: &str, extra: &[&str], guids: &[u64], out_dir: &Path) -> Result<()> {
        if guids.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
        let gf = out_dir.join(format!("export_{kind}_guids.txt"));
        let body: String = guids.iter().map(|g| format!("{g:016x}\n")).collect();
        std::fs::write(&gf, body).map_err(|e| Error::Io(gf.clone(), e))?;
        let mut args: Vec<OsString> = vec!["-nocachedb".into(), "-export".into(), "--exporttypes".into(), kind.into()];
        args.extend(extra.iter().map(OsString::from));
        args.extend(["--exportguids".into(), gf.into_os_string(), "--exportdir".into(), out_dir.into()]);
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, out_dir).map(|_| ())
    }

    /// Exports one material with its textures named by guid, which is what the
    /// library resolves; the default names them by an export-relative path.
    pub fn export_material(&self, paks: &[PathBuf], guid: u64, out_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
        let gf = out_dir.join("export_guids.txt");
        std::fs::write(&gf, format!("{guid:016x}\n")).map_err(|e| Error::Io(gf.clone(), e))?;
        let mut args: Vec<OsString> = vec![
            "-nocachedb".into(),
            "-export".into(),
            "--exporttypes".into(),
            "matl".into(),
            "--texturenames".into(),
            "guid".into(),
            "--exportguids".into(),
            gf.into(),
            "--exportdir".into(),
            out_dir.into(),
        ];
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, out_dir)?;
        Ok(())
    }

    /// Exports one texture as DDS. Setting 3 is "highest mip", written as one
    /// `0x<GUID>_level<n>.dds` per mip so the caller can pick the top one.
    pub fn export_texture(&self, paks: &[PathBuf], guid: u64, out_dir: &Path) -> Result<()> {
        let mut args: Vec<OsString> = vec![
            "-nocachedb".into(),
            "-export".into(),
            "--exporttypes".into(),
            "txtr".into(),
            "--exportsetting".into(),
            "txtr=3".into(),
        ];
        let gf = out_dir.join("export_guids.txt");
        std::fs::create_dir_all(out_dir).map_err(|e| Error::Io(out_dir.to_path_buf(), e))?;
        std::fs::write(&gf, format!("{guid:016x}\n")).map_err(|e| Error::Io(gf.clone(), e))?;
        args.push("--exportguids".into());
        args.push(gf.into());
        args.push("--exportdir".into());
        args.push(out_dir.into());
        args.extend(paks.iter().map(OsString::from));
        self.run(&args, out_dir).map(|_| ())
    }
}
