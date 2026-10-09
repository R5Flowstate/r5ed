//! Turning a validated job request into a plan.
//!
//! This is the only place an external tool's argument vector is written down.
//! Every value that reaches it has already been checked, and the working
//! directory is always inside the project or the cache, so nothing a caller
//! supplies can become a path outside those two trees.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::Config;
use crate::error::{ApiError, Result};
use crate::jobs::{Env, JobKind, JobParams, JobRequest, Plan, Step};
use crate::paths;

/// A map stem is the file name without extension, and it becomes a file name
/// and a command argument, so it is held to the same grammar as a project.
pub fn map_stem(stem: &str) -> Result<&str> {
    if stem.is_empty()
        || stem.len() > 64
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad("a map stem is 1-64 characters of A-Z a-z 0-9 _ -"));
    }
    Ok(stem)
}

/// A compile's target list, in the spelling the CLI takes.
pub fn targets(s: &str) -> Result<String> {
    let mut v: Vec<&str> = Vec::new();
    for t in s.split(',').map(str::trim) {
        if t != "client" && t != "dedi" {
            return Err(ApiError::bad("targets must be a comma separated list of client and dedi"));
        }
        if !v.contains(&t) {
            v.push(t);
        }
    }
    if v.is_empty() {
        return Err(ApiError::bad("a compile job needs at least one target"));
    }
    Ok(v.join(","))
}

/// The navmesh hull presets, with the bounds every one of them is baked over.
/// The bounds come from the project document, so a hull is only a name here.
pub const HULLS: [&str; 5] = ["small", "med_short", "medium", "large", "extra_large"];

fn hull_name(n: &str) -> Result<String> {
    if HULLS.contains(&n) {
        return Ok(n.to_string());
    }
    Err(ApiError::bad(format!("hull must be one of {}", HULLS.join(", "))))
}

/// A bounds box, checked for finiteness and for being inside the world.
fn bounds(b: &[f32; 6]) -> Result<[f32; 6]> {
    for v in b.iter() {
        if !v.is_finite() {
            return Err(ApiError::bad("navmesh bounds must be finite"));
        }
        if v.abs() > 1.0e6 {
            return Err(ApiError::bad("navmesh bounds are outside the world"));
        }
    }
    if b[0] >= b[3] || b[1] >= b[4] || b[2] >= b[5] {
        return Err(ApiError::bad("navmesh bounds are empty"));
    }
    Ok(*b)
}

fn num(v: f32) -> String {
    // Whole numbers print without a fraction, which is what the tool's own
    // examples use and keeps the argv stable.
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn os(s: impl Into<String>) -> OsString {
    OsString::from(s.into())
}

/// The compiler CLI, which every compile-shaped job runs.
fn compile_tool(cfg: &Config) -> Result<PathBuf> {
    cfg.compile_tool()
}

/// The dedicated-server map a project built.
fn dedi_bsp(env: &Env, project: &str, stem: &str) -> Result<PathBuf> {
    let p = env.project_dir(project)?.join("build").join("dedi").join("maps").join(format!("{stem}.bsp"));
    if p.is_file() {
        return Ok(p);
    }
    Ok(env.project_dir(project)?.join("build").join("maps").join(format!("{stem}.bsp")))
}

/// Resolves a map stem to the pak that carries it. The asset index is the only
/// thing that knows, so the caller supplies the lookup and a miss is a miss.
pub type PakLookup<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

/// Builds the plan a job will run. Nothing here is reachable with an
/// unvalidated argument: the request body was parsed into typed fields.
pub fn build(cfg: &Config, env: &Env, req: &JobRequest, pak_for: PakLookup<'_>) -> Result<Plan> {
    crate::paths::project_name(&req.project)?;
    let project_dir = env.project_dir(&req.project)?;
    if !project_dir.is_dir() {
        return Err(ApiError::not_found(format!("no project {}", req.project)));
    }
    let build_dir = project_dir.join("build");
    let p = &req.params;
    let mut steps = Vec::new();
    let tool = compile_tool(cfg);

    match req.kind {
        JobKind::Compile => {
            let t = targets(&p.targets)?;
            steps.push(Step::Tool {
                program: tool?,
                args: vec![
                    os("build"),
                    os(project_dir.to_string_lossy().into_owned()),
                    os("--out"),
                    os(build_dir.to_string_lossy().into_owned()),
                    os("--targets"),
                    os(t),
                ],
            });
        }
        JobKind::Bake => steps.push(Step::Tool {
            program: tool?,
            args: vec![os("bake"), os(project_dir.to_string_lossy().into_owned())],
        }),
        JobKind::Package => steps.push(Step::Tool {
            program: tool?,
            args: vec![os("package"), os(project_dir.to_string_lossy().into_owned())],
        }),
        JobKind::ImportTf2 => {
            let stem = map_stem(&p.stem)?;
            let src = paths::contained(&env.cache_root, &p.source_bsp)?;
            if !src.is_file() {
                return Err(ApiError::not_found("the source bsp is not in the cache"));
            }
            steps.push(Step::Tool {
                program: tool?,
                args: vec![
                    os("import-tf2"),
                    os(src.to_string_lossy().into_owned()),
                    os(project_dir.to_string_lossy().into_owned()),
                    os("--stem"),
                    os(stem),
                ],
            });
        }
        JobKind::ExportMinimap | JobKind::ExportLoadscreen => {
            let sub = if req.kind == JobKind::ExportMinimap { "minimap" } else { "loadscreen" };
            steps.push(Step::Tool {
                program: tool?,
                args: vec![os(sub), os(project_dir.to_string_lossy().into_owned())],
            });
        }
        JobKind::Navmesh => {
            let recast = cfg
                .tools
                .recast
                .clone()
                .filter(|p| p.is_file())
                .ok_or_else(|| ApiError::Unavailable("the recast tool is not configured".into()))?;
            let stem = map_stem(&p.stem)?;
            let bsp = dedi_bsp(env, &req.project, stem)?;
            let hulls = if p.hulls.is_empty() {
                vec![crate::jobs::Hull { name: "small".into(), bounds: [-32768.0; 6] }]
            } else {
                if p.hulls.len() > 8 {
                    return Err(ApiError::bad("at most 8 hulls per navmesh job"));
                }
                p.hulls.clone()
            };
            for h in hulls {
                let name = hull_name(&h.name)?;
                let b = bounds(&h.bounds)?;
                let mut args = vec![os("-console"), os(bsp.to_string_lossy().into_owned()), os("--bounds")];
                args.extend(b.iter().map(|v| os(num(*v))));
                args.push(os("--hull"));
                args.push(os(name));
                steps.push(Step::Tool { program: recast.clone(), args });
            }
        }
        JobKind::Deploy => {
            for f in &p.files {
                crate::deploy::target_allowed(f).then_some(()).ok_or_else(|| {
                    ApiError::bad(format!("{f} is not under a deployable directory"))
                })?;
                paths::relative_path(f)?;
            }
            steps.push(Step::Deploy { files: p.files.clone(), replace_shipped: p.replace_shipped });
        }
        JobKind::Undeploy => {
            if p.manifest.is_empty() {
                return Err(ApiError::bad("an undeploy job needs a manifest id"));
            }
            crate::deploy::read_manifest(&env.cache_root, &p.manifest)?;
            steps.push(Step::Undeploy { manifest: p.manifest.clone() });
        }
        JobKind::Reload => {
            // The map change goes through the agent link, not a process.
            let stem = map_stem(&p.stem)?;
            let code = crate::live::protocol::script(&crate::live::protocol::Checked::Reload {
                stem: stem.to_string(),
            });
            steps.push(Step::Live { code });
        }
        JobKind::ImportMap => {
            cfg.tools
                .rsx
                .clone()
                .filter(|p| p.is_file())
                .ok_or_else(|| ApiError::Unavailable("the asset tool is not configured".into()))?;
            // A pak picked in the browser names its own map; otherwise the
            // install's pak for the requested stem is used.
            let (stem, pak) = if p.pak.is_empty() {
                let stem = map_stem(&p.stem)?.to_string();
                let pak = pak_for(&stem)
                    .ok_or_else(|| ApiError::not_found(format!("the install carries no map called {stem}")))?;
                (stem, pak)
            } else {
                let pak = std::path::PathBuf::from(&p.pak);
                let stem = crate::browse::stem_of_pak(&pak)
                    .ok_or_else(|| ApiError::bad("pick a map's *_perm.rpak or *_temp.rpak"))?;
                if !pak.is_file() {
                    return Err(ApiError::not_found(format!("{} is not a file", pak.display())));
                }
                (map_stem(&stem)?.to_string(), pak)
            };
            steps.push(Step::Tool {
                program: tool?,
                args: crate::jobs::import_map_argv(&stem, &project_dir, &pak)?,
            });
        }
    }
    // Every step runs in the project's build tree, created here so the working
    // directory does not depend on whether a previous compile left one behind.
    std::fs::create_dir_all(&build_dir)
        .map_err(|e| ApiError::Internal(format!("{}: {e}", build_dir.display())))?;
    // The compiler resolves the game and its tools from these, exactly as a
    // shell run of it would.
    let mut env: Vec<(String, std::path::PathBuf)> = vec![("R5ED_GAME_ROOT".into(), cfg.game_root.clone())];
    for (key, tool) in [("R5ED_RSX", &cfg.tools.rsx), ("R5ED_REPAK", &cfg.tools.repak), ("R5ED_REVPK", &cfg.tools.revpk), ("R5ED_RECAST", &cfg.tools.recast)] {
        if let Some(p) = tool.as_ref().filter(|p| p.is_file()) {
            env.push((key.into(), p.clone()));
        }
    }
    Ok(Plan { steps, cwd: build_dir, timeout: Duration::from_secs(1800), env })
}

/// The request body as it arrives on the wire.
#[derive(Clone, Debug, serde::Deserialize)]
struct RawRequest {
    kind: String,
    project: String,
    #[serde(default)]
    params: JobParams,
}

impl TryFrom<RawRequest> for JobRequest {
    type Error = ApiError;
    fn try_from(r: RawRequest) -> Result<Self> {
        Ok(Self { kind: JobKind::parse(&r.kind)?, project: r.project, params: r.params })
    }
}

/// Parses a request body, refusing anything the enum or the path rules reject.
pub fn parse_request(body: &[u8]) -> Result<JobRequest> {
    if body.len() > crate::secure::MAX_JSON_BYTES {
        return Err(ApiError::BadRequest("job request is too large".into()));
    }
    let raw: RawRequest =
        serde_json::from_slice(body).map_err(|e| ApiError::bad(format!("malformed job request: {e}")))?;
    JobRequest::try_from(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stem_and_a_target_list_are_validated() {
        assert!(map_stem("mp_rr_gauntlet").is_ok());
        for bad in ["", "a/b", "a b", "a.b", &"x".repeat(65)] {
            assert!(map_stem(bad).is_err(), "{bad:?}");
        }
        assert_eq!(targets("client,dedi").unwrap(), "client,dedi");
        assert_eq!(targets(" dedi , client ,dedi").unwrap(), "dedi,client");
        for bad in ["", "client,nope", "nope", "client,", ".."] {
            assert!(targets(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn navmesh_bounds_must_be_a_non_degenerate_finite_box() {
        assert!(bounds(&[-100.0, -100.0, -50.0, 100.0, 100.0, 50.0]).is_ok());
        assert!(bounds(&[0.0, 0.0, 0.0, 0.0, 10.0, 10.0]).is_err(), "empty on x");
        assert!(bounds(&[0.0, 0.0, 0.0, 10.0, 0.0, 10.0]).is_err(), "empty on y");
        assert!(bounds(&[0.0, 0.0, 0.0, 10.0, 10.0, 0.0]).is_err(), "empty on z");
        assert!(bounds(&[f32::NAN, 0.0, 0.0, 1.0, 1.0, 1.0]).is_err());
        assert!(bounds(&[0.0, 0.0, 0.0, f32::INFINITY, 1.0, 1.0]).is_err());
        assert!(bounds(&[0.0, 0.0, 0.0, 1.0e7, 1.0, 1.0]).is_err());
    }

    #[test]
    fn whole_bounds_print_without_a_fraction() {
        assert_eq!(num(-32768.0), "-32768");
        assert_eq!(num(1.5), "1.5");
    }

    /// A config whose compile tool is a file that exists, so a plan can be
    /// built without an install.
    fn cfg_with(tool: PathBuf) -> Config {
        let mut c = Config::default();
        c.game_root = std::env::temp_dir();
        c.tools.compile = Some(tool);
        c
    }

    #[test]
    fn a_compile_plan_is_the_documented_argv() {
        let dir = std::env::temp_dir().join("r5ed-host-plan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("projects/m")).unwrap();
        std::fs::write(dir.join("tool.exe"), b"x").unwrap();
        let env = Env {
            game_root: dir.join("game"),
            projects_root: dir.join("projects"),
            cache_root: dir.join("cache"),
        };
        std::fs::create_dir_all(&env.game_root).unwrap();
        let cfg = cfg_with(dir.join("tool.exe"));
        let req = JobRequest {
            kind: JobKind::Compile,
            project: "m".into(),
            params: JobParams { targets: "client,dedi".into(), ..Default::default() },
        };
        let plan = build(&cfg, &env, &req, &|_| None).unwrap();
        let (program, args) = plan.first_argv().unwrap();
        assert_eq!(program, dir.join("tool.exe"));
        let got: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(got[0], "build");
        assert!(got[1].ends_with("m"), "{}", got[1]);
        assert!(got[1].contains("projects"), "{}", got[1]);
        assert_eq!(got[2], "--out");
        assert!(got[3].ends_with("build"), "{}", got[3]);
        assert!(got[3].contains("projects"), "{}", got[3]);
        assert_eq!(got[4], "--targets");
        assert_eq!(got[5], "client,dedi");
        assert_eq!(got.len(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_body_is_parsed_into_typed_fields() {
        let r = parse_request(br#"{"kind":"compile","project":"m","params":{"targets":"client"}}"#).unwrap();
        assert_eq!(r.kind, JobKind::Compile);
        assert_eq!(r.project, "m");
        assert_eq!(r.params.targets, "client");
        assert!(parse_request(br#"{"kind":"nope","project":"m"}"#).is_err());
        assert!(parse_request(b"not json").is_err());
        assert!(parse_request(br#"{"project":"m"}"#).is_err());
        assert!(parse_request(&vec![b'x'; 2 << 20]).is_err());
    }
}
