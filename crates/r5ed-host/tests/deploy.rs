//! Deploying into an install and taking it back out.

mod common;

use r5ed_host::deploy::{self, Manifest, ALLOWED};
use r5ed_host::jobs::LogBuffer;

mod rbsp_stub {
    use std::path::Path;

    /// A minimal map the container reader accepts, so the deploy path can be
    /// exercised without a real compile.
    pub fn write(path: &Path, version: u16) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).expect("parent");
        }
        let bsp = rbsp::Bsp::new(version, rbsp::Version::from_header(version).expect("version").flags(), 0);
        bsp.write_path(path).expect("write bsp");
    }
}

struct Fixture {
    env: r5ed_host::jobs::Env,
    _dir: std::path::PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("r5ed-deploy-test-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let env = r5ed_host::jobs::Env {
        game_root: dir.join("game"),
        projects_root: dir.join("projects"),
        cache_root: dir.join("cache"),
    };
    for d in [&env.game_root, &env.projects_root, &env.cache_root] {
        std::fs::create_dir_all(d).expect("root");
    }
    std::fs::create_dir_all(env.project_dir("m").expect("project dir")).expect("project");
    Fixture { env, _dir: dir }
}

#[test]
fn deploy_backup_and_undeploy() {
    let f = fixture("roundtrip");
    let build = f.env.project_dir("m").expect("project").join("build");

    // Two targets: one that replaces a file the install already had, one that
    // is new.
    rbsp_stub::write(&build.join("maps").join("mp_new.bsp"), 51);
    std::fs::create_dir_all(build.join("platform").join("cfg")).expect("build cfg");
    std::fs::write(build.join("platform").join("cfg").join("srv.cfg"), b"new cfg\n").expect("cfg");
    let shipped = f.env.game_root.join("platform").join("cfg").join("srv.cfg");
    std::fs::create_dir_all(shipped.parent().unwrap()).expect("cfg dir");
    std::fs::write(&shipped, b"shipped cfg\n").expect("shipped");

    let log = LogBuffer::default();
    let report = deploy::deploy(
        &f.env,
        "m",
        &["maps/mp_new.bsp".to_string(), "platform/cfg/srv.cfg".to_string()],
        true,
        &log,
    )
    .expect("deploy");

    assert_eq!(report.written.len(), 2);
    let new = report.written.iter().find(|w| w.target == "maps/mp_new.bsp").expect("new file");
    assert!(new.created, "the map did not exist before");
    assert!(new.backup.is_none());
    let old = report.written.iter().find(|w| w.target == "platform/cfg/srv.cfg").expect("cfg");
    assert!(!old.created);
    let backup = old.backup.clone().expect("a backup");
    assert!(backup.starts_with("backups/"), "{backup}");
    // The bytes are in the install and the prior bytes are in the cache.
    assert!(f.env.game_root.join("maps").join("mp_new.bsp").is_file());
    assert_eq!(std::fs::read_to_string(f.env.cache_root.join(&backup)).expect("backup"), "shipped cfg\n");
    assert_eq!(std::fs::read_to_string(&shipped).expect("installed"), "new cfg\n");

    // The manifest names every file and the deploy can be read back.
    let m = deploy::read_manifest(&f.env.cache_root, &report.manifest).expect("manifest");
    assert_eq!(m.project, "m");
    assert_eq!(m.files.len(), 2);
    assert_eq!(m, Manifest { stamp: m.stamp, project: "m".into(), files: report.written.clone() });
    assert_eq!(deploy::manifest_id(m.stamp), report.manifest, "the id is the deploy's stamp");

    // Undeploy restores the prior bytes and removes what it created.
    let n = deploy::undeploy(&f.env, &report.manifest, &log).expect("undeploy");
    assert_eq!(n, 2);
    assert_eq!(std::fs::read_to_string(&shipped).expect("restored"), "shipped cfg\n");
    assert!(!f.env.game_root.join("maps").join("mp_new.bsp").exists());
    // The manifest is consumed, so a second undeploy cannot run twice.
    assert!(deploy::undeploy(&f.env, &report.manifest, &log).is_err());
}

#[test]
fn deploy_refuses_unknown_overwrite() {
    let f = fixture("refuse");
    let build = f.env.project_dir("m").expect("project").join("build");
    std::fs::create_dir_all(build.join("platform").join("cfg")).expect("build cfg");
    std::fs::write(build.join("platform").join("cfg").join("srv.cfg"), b"mine\n").expect("cfg");
    let shipped = f.env.game_root.join("platform").join("cfg").join("srv.cfg");
    std::fs::create_dir_all(shipped.parent().unwrap()).expect("cfg dir");
    std::fs::write(&shipped, b"theirs\n").expect("shipped");

    let log = LogBuffer::default();
    let r = deploy::deploy(&f.env, "m", &["platform/cfg/srv.cfg".to_string()], false, &log);
    assert!(r.is_err(), "{r:?}");
    assert_eq!(std::fs::read_to_string(&shipped).expect("untouched"), "theirs\n");

    // A file this host has written before is fair game without the flag.
    let forced = deploy::deploy(&f.env, "m", &["platform/cfg/srv.cfg".to_string()], true, &log).expect("forced");
    let again = deploy::deploy(&f.env, "m", &["platform/cfg/srv.cfg".to_string()], false, &log).expect("second");
    assert_eq!(std::fs::read_to_string(&shipped).expect("overwritten"), "mine\n");
    assert!(again.written[0].backup.is_some());
    // Undoing the second deploy brings back what the first one wrote.
    deploy::undeploy(&f.env, &again.manifest, &log).expect("undeploy");
    assert_eq!(std::fs::read_to_string(&shipped).expect("back"), "mine\n");
    deploy::undeploy(&f.env, &forced.manifest, &log).expect("undeploy first");
    assert_eq!(std::fs::read_to_string(&shipped).expect("original"), "theirs\n");
}

#[test]
fn deploy_only_touches_allowlisted_directories() {
    let f = fixture("allowlist");
    let build = f.env.project_dir("m").expect("project").join("build");
    for rel in ["bin/evil.exe", "paks/other.rpak", "../outside.txt", "maps/../../out.bsp"] {
        let src = build.join(rel.replace('/', "\\"));
        if let Some(d) = src.parent() {
            std::fs::create_dir_all(d).expect("dir");
        }
        std::fs::write(&src, b"x").expect("write");
        let log = LogBuffer::default();
        let r = deploy::deploy(&f.env, "m", &[rel.to_string()], true, &log);
        assert!(r.is_err(), "{rel} should be refused");
    }
    // Nothing escaped the install root.
    assert!(!f.env.game_root.join("bin").exists());
    assert!(!f.env.project_dir("m").unwrap().parent().unwrap().join("outside.txt").exists());
    for a in ALLOWED {
        assert!(deploy::target_allowed(&format!("{a}/x")), "{a}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_deploy_routes_run_as_jobs() {
    use common::HarnessBuilder;
    let h = HarnessBuilder::new("deploy-api").start().await.expect("harness");
    let build = h.config.projects_root.join("m").join("build");
    std::fs::create_dir_all(build.join("maps")).expect("build maps");
    rbsp_stub::write(&build.join("maps").join("mp_api.bsp"), 47);
    std::fs::create_dir_all(h.config.game_root.join("maps")).expect("game maps");

    let r = h.post("/api/jobs", br#"{"kind":"deploy","project":"m","params":{"files":["maps/mp_api.bsp"]}}"#);
    assert_eq!(r.status, 200, "{}", r.text());
    let id: serde_json::Value = serde_json::from_str(&r.text()).expect("id");
    let v = h.wait_job(id["id"].as_str().expect("id")).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Succeeded, "{v:?}");
    assert!(h.config.game_root.join("maps").join("mp_api.bsp").is_file());
    // The deploy's own stamp is the id, so the artifact an undeploy job needs
    // is exactly what the deploy reported.
    let manifest = v
        .artifacts
        .iter()
        .find(|a| a.chars().all(|c| c.is_ascii_digit()))
        .expect("a manifest artifact");

    let r = h.post("/api/jobs", format!(r#"{{"kind":"undeploy","project":"m","params":{{"manifest":"{manifest}"}}}}"#).as_bytes());
    assert_eq!(r.status, 200, "{}", r.text());
    let id: serde_json::Value = serde_json::from_str(&r.text()).expect("id");
    let v = h.wait_job(id["id"].as_str().expect("id")).await;
    assert_eq!(v.state, r5ed_host::jobs::JobState::Succeeded, "{v:?}");
    assert!(!h.config.game_root.join("maps").join("mp_api.bsp").exists());

    // A deploy that names a file outside the allowlist never becomes a job.
    let r = h.post("/api/jobs", br#"{"kind":"deploy","project":"m","params":{"files":["bin/evil.exe"]}}"#);
    assert_eq!(r.status, 400, "{}", r.text());
    // A job for a project that does not exist is refused too.
    let r = h.post("/api/jobs", br#"{"kind":"compile","project":"nope","params":{"targets":"client"}}"#);
    assert_eq!(r.status, 404, "{}", r.text());
}
