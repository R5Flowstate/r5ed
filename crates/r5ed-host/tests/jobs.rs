//! The job runner: the exact argv, the timeout, and cancellation.

mod common;

use std::time::Duration;

use common::{fake_tool, HarnessBuilder};
use r5ed_host::jobs::{JobKind, JobParams, JobRequest, Plan, Step};
use r5ed_host::plan;

/// Builds a plan the way the API does, from a real request body.
fn plan_for(h: &common::Harness, body: &str) -> Plan {
    let req = plan::parse_request(body.as_bytes()).expect("request parses");
    plan::build(&h.config, &h.env(), &req, &|_| None).expect("plan builds")
}

#[tokio::test(flavor = "multi_thread")]
async fn job_argv_exact() {
    let h = HarnessBuilder::new("argv").tools().start().await.expect("harness");
    // The projects root has to hold a real project for a plan to be legal.
    let dir = h.config.projects_root.clone();
    std::fs::create_dir_all(dir.join("m")).expect("project");

    // compile: build <project_dir> --out <project_dir>/build --targets client,dedi
    let p = plan_for(
        &h,
        r#"{"kind":"compile","project":"m","params":{"targets":"client,dedi"}}"#,
    );
    let (program, args) = p.first_argv().expect("a tool step");
    assert_eq!(program, fake_tool());
    let got: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let proj = dir.join("m").to_string_lossy().into_owned();
    assert_eq!(
        got,
        vec![
            "build".to_string(),
            proj.clone(),
            "--out".to_string(),
            dir.join("m").join("build").to_string_lossy().into_owned(),
            "--targets".to_string(),
            "client,dedi".to_string(),
        ]
    );
    assert_eq!(p.cwd, dir.join("m").join("build"), "the working dir is the build tree");

    // bake and package are one verb plus the project.
    let p = plan_for(&h, r#"{"kind":"bake","project":"m"}"#);
    let (_, a) = p.first_argv().unwrap();
    assert_eq!(a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["bake".to_string(), proj.clone()]);
    let p = plan_for(&h, r#"{"kind":"package","project":"m"}"#);
    let (_, a) = p.first_argv().unwrap();
    assert_eq!(a.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["package".to_string(), proj.clone()]);

    // The map asset routes are the same shape.
    let p = plan_for(&h, r#"{"kind":"export_minimap","project":"m"}"#);
    let (_, a) = p.first_argv().unwrap();
    assert_eq!(a[0].to_string_lossy(), "minimap");
    let p = plan_for(&h, r#"{"kind":"export_loadscreen","project":"m"}"#);
    let (_, a) = p.first_argv().unwrap();
    assert_eq!(a[0].to_string_lossy(), "loadscreen");

    // navmesh: one invocation per hull, in the order given, each with the
    // bounds the project declared. The map only has to exist to name it.
    let bsp = dir.join("m").join("build").join("dedi").join("maps").join("mp_x.bsp");
    std::fs::create_dir_all(bsp.parent().unwrap()).unwrap();
    std::fs::write(&bsp, rbsp::Bsp::new(47, 0, 0).to_monolithic()).unwrap();
    let body = r#"{"kind":"navmesh","project":"m","params":{"stem":"mp_x","hulls":[
        {"name":"small","bounds":[-100.0,-50.0,-25.0,100.0,50.0,25.0]},
        {"name":"medium","bounds":[-1.0,-2.0,-3.0,4.0,5.0,6.0]}]}}"#
        .replace('\n', "");
    let p = plan_for(&h, &body);
    assert_eq!(p.steps.len(), 2, "one step per hull");
    for step in &p.steps {
        let Step::Tool { program, args } = step else { panic!("expected a tool step") };
        assert_eq!(program, &fake_tool());
        let a: Vec<String> = args.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert_eq!(a[0], "-console");
        assert!(a[1].ends_with("mp_x.bsp"), "{}", a[1]);
        assert_eq!(a[2], "--bounds");
        assert!(a[a.len() - 2] == "--hull");
    }
    let first: Vec<String> = match &p.steps[0] {
        Step::Tool { args, .. } => args.iter().map(|x| x.to_string_lossy().into_owned()).collect(),
        _ => unreachable!(),
    };
    assert_eq!(first[3], "-100");
    assert_eq!(first[4], "-50");
    assert_eq!(first[5], "-25");
    assert_eq!(first[6], "100");
    assert_eq!(first[7], "50");
    assert_eq!(first[8], "25");
    assert_eq!(first[9], "--hull");
    assert_eq!(first[10], "small");

    // A hull that is not one of the presets, and bounds that are not a box,
    // are both refused before any process exists.
    let bad = r#"{"kind":"navmesh","project":"m","params":{"stem":"mp_x","hulls":[{"name":"huge","bounds":[0,0,0,1,1,1]}]}}"#;
    assert!(plan::parse_request(bad.as_bytes()).is_ok());
    assert!(plan::build(&h.config, &h.env(), &plan::parse_request(bad.as_bytes()).unwrap(), &|_| None).is_err());
    let bad2 = r#"{"kind":"navmesh","project":"m","params":{"stem":"mp_x","hulls":[{"name":"small","bounds":[0,0,0,0,1,1]}]}}"#;
    assert!(plan::build(&h.config, &h.env(), &plan::parse_request(bad2.as_bytes()).unwrap(), &|_| None).is_err());

    // A target list that names something else never reaches a process.
    let bad3 = r#"{"kind":"compile","project":"m","params":{"targets":"client,rm"}}"#;
    assert!(plan::build(&h.config, &h.env(), &plan::parse_request(bad3.as_bytes()).unwrap(), &|_| None).is_err());

    // import_map: one compiler run, given the map's client pak; the compiler
    // exports the pak and its _temp partner itself.
    let pak = h.dir.join("paks").join("Win64").join("mp_x_client_perm.rpak");
    std::fs::create_dir_all(pak.parent().unwrap()).unwrap();
    std::fs::write(&pak, b"pak").unwrap();
    let req = plan::parse_request(br#"{"kind":"import_map","project":"m","params":{"stem":"mp_x"}}"#).expect("request");
    let p = plan::build(&h.config, &h.env(), &req, &|stem| (stem == "mp_x").then(|| pak.clone())).expect("plan");
    assert_eq!(p.steps.len(), 1, "{:?}", p.steps);
    let (_, args) = p.first_argv().expect("a tool step");
    let a: Vec<String> = args.iter().map(|x| x.to_string_lossy().into_owned()).collect();
    assert_eq!(a[0], "import-map");
    assert_eq!(a[1], "mp_x");
    assert_eq!(a[3], "--from");
    assert_eq!(a[4], pak.to_string_lossy());
    assert_eq!(a[5], "--force");

    // A pak picked in the browser names the map itself.
    let body = format!(r#"{{"kind":"import_map","project":"m","params":{{"pak":{:?}}}}}"#, pak.to_string_lossy().replace('\\', "/"));
    let p = plan::build(&h.config, &h.env(), &plan::parse_request(body.as_bytes()).expect("request"), &|_| None).expect("plan");
    let (_, args) = p.first_argv().expect("a tool step");
    let a: Vec<String> = args.iter().map(|x| x.to_string_lossy().into_owned()).collect();
    assert_eq!(a[1], "mp_x", "the stem comes from the pak's name");
    // A pak that is not a map's client pak never becomes a job.
    let body = format!(r#"{{"kind":"import_map","project":"m","params":{{"pak":{:?}}}}}"#, h.dir.join("common.rpak").to_string_lossy().replace('\\', "/"));
    assert!(plan::build(&h.config, &h.env(), &plan::parse_request(body.as_bytes()).unwrap(), &|_| None).is_err());

    // A map the install does not carry never becomes a job.
    let p = plan::build(&h.config, &h.env(), &req, &|_| None);
    assert!(p.is_err(), "{p:?}");

    // And the job kinds that need no program say so by having no tool step.
    let p = plan_for(&h, r#"{"kind":"reload","project":"m","params":{"stem":"mp_x"}}"#);
    assert!(p.first_argv().is_none());
    assert_eq!(p.steps.len(), 1, "a reload is one agent link line");
    assert!(matches!(&p.steps[0], Step::Live { code } if code.contains("mp_x")), "{:?}", p.steps);
}

#[tokio::test(flavor = "multi_thread")]
async fn job_timeout_kills() {
    let h = HarnessBuilder::new("timeout").tools().start().await.expect("harness");
    let out = h.config.cache_root.join("argv.txt");
    // A child that would otherwise run for a minute, under a 300 ms cap.
    let plan = Plan {
        steps: vec![Step::Tool {
            program: fake_tool(),
            args: vec![
                "--record".into(),
                out.to_string_lossy().into_owned().into(),
                "--sleep".into(),
                "60000".into(),
            ],
        }],
        cwd: h.config.cache_root.clone(),
        timeout: Duration::from_millis(300),
        env: Vec::new(),
    };
    let job = h.jobs.submit(plan, JobKind::Bake, "m".into());
    let v = h.wait_job(&job.id).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Failed, "{v:?}");
    let err = v.error.clone().expect("a reason");
    assert!(err.contains("killed"), "{err}");
    // The log records that the tool was started and never reported progress.
    assert!(v.log.iter().any(|l| l.text.starts_with("$ ")), "{:?}", v.log);
    assert!(out.is_file(), "the child really ran");
}

#[tokio::test(flavor = "multi_thread")]
async fn job_cancel() {
    let h = HarnessBuilder::new("cancel").tools().start().await.expect("harness");
    let out = h.config.cache_root.join("cancel.txt");
    let plan = Plan {
        steps: vec![Step::Tool {
            program: fake_tool(),
            args: vec![
                "--record".into(),
                out.to_string_lossy().into_owned().into(),
                "--progress".into(),
                "10".into(),
                "--sleep".into(),
                "60000".into(),
            ],
        }],
        cwd: h.config.cache_root.clone(),
        timeout: Duration::from_secs(120),
        env: Vec::new(),
    };
    let job = h.jobs.submit(plan, JobKind::Compile, "m".into());
    // Wait until the child is actually up, so the cancel lands on a live job.
    for _ in 0..200 {
        if out.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(out.is_file(), "the child started");
    let r = h.post(&format!("/api/jobs/{}/cancel", job.id), b"");
    assert_eq!(r.status, 200, "{}", r.text());
    let v = h.wait_job(&job.id).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Cancelled, "{v:?}");
    // Cancelling a job that does not exist is a 404, not a silent success.
    assert_eq!(h.post("/api/jobs/00000000-0000-0000-0000-000000000000/cancel", b"").status, 404);

    // A job that finishes on its own reports success and 100 percent.
    let plan = Plan {
        steps: vec![Step::Tool {
            program: fake_tool(),
            args: vec!["--progress".into(), "40".into(), "--exit".into(), "0".into()],
        }],
        cwd: h.config.cache_root.clone(),
        timeout: Duration::from_secs(30),
        env: Vec::new(),
    };
    let job = h.jobs.submit(plan, JobKind::Package, "m".into());
    let v = h.wait_job(&job.id).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Succeeded, "{v:?}");
    assert_eq!(v.progress, 100);
    assert!(v.log.iter().any(|l| l.text == "PROGRESS 40"), "{:?}", v.log.iter().map(|l| &l.text).collect::<Vec<_>>());

    // A tool that fails fails the job and keeps the reason.
    let plan = Plan {
        steps: vec![Step::Tool { program: fake_tool(), args: vec!["--exit".into(), "3".into()] }],
        cwd: h.config.cache_root.clone(),
        timeout: Duration::from_secs(30),
        env: Vec::new(),
    };
    let job = h.jobs.submit(plan, JobKind::Bake, "m".into());
    let v = h.wait_job(&job.id).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Failed);
    let why = v.error.clone().unwrap_or_default();
    assert!(why.contains("exited"), "{v:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_job_routes_carry_a_closed_kind_enum() {
    let h = HarnessBuilder::new("kinds").tools().start().await.expect("harness");
    std::fs::create_dir_all(h.config.projects_root.join("m")).unwrap();
    let r = h.post("/api/jobs", br#"{"kind":"rm -rf","project":"m"}"#);
    assert_eq!(r.status, 400, "{}", r.text());
    let r = h.post("/api/jobs", br#"{"kind":"compile","project":"m","params":{"targets":"client"}}"#);
    assert_eq!(r.status, 200, "{}", r.text());
    let id: serde_json::Value = serde_json::from_str(&r.text()).expect("id");
    let id = id["id"].as_str().expect("an id").to_string();
    let v = h.get(&format!("/api/jobs/{id}"));
    assert_eq!(v.status, 200);
    let j: serde_json::Value = serde_json::from_str(&v.text()).expect("job view");
    assert_eq!(j["kind"], "compile");
    assert!(j["log"].is_array());
    assert!(j["artifacts"].is_array());
    assert!(j["state"].is_string());
    let _ = h.wait_job(&id).await;
    let v = h.get("/api/jobs/00000000-0000-0000-0000-000000000000");
    assert_eq!(v.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_never_names_a_program_the_caller_chose() {
    // The kind enum is the only way in, and every kind resolves its own tool.
    let h = HarnessBuilder::new("closed").start().await.expect("harness");
    std::fs::create_dir_all(h.config.projects_root.join("m")).unwrap();
    for kind in JobKind::ALL {
        let body = format!(
            "{{\"kind\":\"{}\",\"project\":\"m\",\"params\":{{\"stem\":\"mp_x\"}}}}",
            kind.as_str()
        );
        let req = match plan::parse_request(body.as_bytes()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // With no tools configured the only kinds that can build a plan are the
        // ones that need no external program.
        match plan::build(&h.config, &h.env(), &req, &|_| None) {
            Ok(p) => {
                for s in &p.steps {
                    if let Step::Tool { program, .. } = s {
                        assert!(h.config.tools.compile.as_deref() == Some(program.as_path())
                            || h.config.tools.recast.as_deref() == Some(program.as_path())
                            || h.config.tools.rsx.as_deref() == Some(program.as_path()),
                            "{} tried to run {program:?}", kind.as_str());
                    }
                }
            }
            Err(_) => continue,
        }
    }
}

/// A request with a program-shaped parameter must be ignored, not honoured.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_body_cannot_smuggle_a_program() {
    let h = HarnessBuilder::new("smuggle").tools().start().await.expect("harness");
    std::fs::create_dir_all(h.config.projects_root.join("m")).unwrap();
    let r = h.post(
        "/api/jobs",
        br#"{"kind":"compile","project":"m","params":{"targets":"client","program":"cmd.exe","args":"/c calc"}}"#,
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let id: serde_json::Value = serde_json::from_str(&r.text()).unwrap();
    let v = h.wait_job(id["id"].as_str().unwrap()).await;
    // It ran the configured compiler, not what the body asked for.
    let log: Vec<String> = v.log.iter().map(|l| l.text.clone()).collect();
    assert!(log.iter().any(|l| l.contains("build") && l.contains("--targets")), "{log:?}");
    assert!(!log.iter().any(|l| l.contains("cmd.exe") || l.contains("calc")), "{log:?}");
    let _ = JobParams::default();
    let _ = JobRequest { kind: JobKind::Bake, project: "m".into(), params: JobParams::default() };
}

#[test]
fn the_job_kind_enum_is_closed_both_ways() {
    for k in r5ed_host::jobs::JobKind::ALL {
        assert_eq!(r5ed_host::jobs::JobKind::parse(k.as_str()).unwrap(), k);
    }
    assert!(r5ed_host::jobs::JobKind::parse("rm -rf").is_err());
    assert!(r5ed_host::jobs::JobKind::parse("").is_err());
    assert!(r5ed_host::jobs::JobKind::parse("COMPILE").is_err());
    assert_eq!(r5ed_host::jobs::JobKind::ALL.len(), 11);
    assert!(r5ed_host::jobs::JobKind::Compile.is_heavy());
    assert!(!r5ed_host::jobs::JobKind::Reload.is_heavy());
}

#[test]
fn progress_lines_are_parsed_and_bounded() {
    use r5ed_host::jobs::parse_progress;
    assert_eq!(parse_progress("PROGRESS 0"), Some(0));
    assert_eq!(parse_progress("PROGRESS 100"), Some(100));
    assert_eq!(parse_progress("  PROGRESS   42  "), Some(42));
    assert_eq!(parse_progress("PROGRESS 101"), None);
    assert_eq!(parse_progress("PROGRESS -1"), None);
    assert_eq!(parse_progress("PROGRESS"), None);
    assert_eq!(parse_progress("PROGRESS 20 compiling"), Some(20));
    assert_eq!(parse_progress("PROGRESS 45 baking lightmaps: 41% (12/30 texels), 6m00s elapsed"), Some(45));
    assert_eq!(parse_progress("PROGRESSIVE 5"), None);
    assert_eq!(parse_progress("progress 5"), None);
    assert_eq!(parse_progress("we printed PROGRESS 5 earlier"), None);
}

#[test]
fn the_ring_keeps_the_newest_lines_and_the_last_progress() {
    use r5ed_host::jobs::{LogBuffer, LOG_RING};
    let b = LogBuffer::default();
    for i in 0..(LOG_RING + 10) {
        b.push(format!("line {i}"));
    }
    b.push("PROGRESS 30".into());
    b.push("PROGRESS 90".into());
    assert_eq!(b.progress(), 90);
    let t = b.tail(0, LOG_RING);
    assert_eq!(t.len(), LOG_RING);
    assert_eq!(t.last().unwrap().text, "PROGRESS 90");
    let half = (LOG_RING + 10) as u64 / 2;
    let newer = b.tail(half, 10);
    assert!(newer.iter().all(|l| l.seq >= half));
    assert!(newer.len() <= 10);
}

/// An import job runs the compiler with exactly this vector, and a stem or a
/// project name the launcher rules refuse never reaches it.
#[test]
fn the_import_map_argv_is_the_verbs_own() {
    use std::path::Path;
    let pak = Path::new("paks/Win64/mp_rr_x_client_perm.rpak");
    let argv = r5ed_host::jobs::import_map_argv("mp_rr_x", Path::new("projects/mp_rr_x"), pak).unwrap();
    let got: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(got, vec!["import-map", "mp_rr_x", "projects/mp_rr_x", "--from", "paks/Win64/mp_rr_x_client_perm.rpak", "--force"]);
    assert!(r5ed_host::jobs::import_map_argv("", Path::new("projects/m"), pak).is_err());
    assert!(r5ed_host::jobs::import_map_argv("a b", Path::new("projects/m"), pak).is_err());
    assert!(r5ed_host::jobs::import_map_argv("../x", Path::new("projects/m"), pak).is_err());
    assert!(r5ed_host::jobs::import_map_argv("mp_rr_x", Path::new("projects/bad name"), pak).is_err());
}
