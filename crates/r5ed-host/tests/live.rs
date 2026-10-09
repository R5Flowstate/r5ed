//! The live link: message validation, the exact script text, and a round trip
//! against a stand-in for the server's agent socket.

mod common;

use std::time::Duration;

use r5ed_host::live::agent::Link;
use r5ed_host::live::protocol::{self, Message, Target, LIMIT};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn live_message_validation() {
    let u = id();
    // Everything a well-formed frame can say.
    let good = [
        Message::Prop { id: u.clone(), model: "mdl/props/a/b.rmdl".into(), origin: [1.0, 2.0, 3.0], angles: [0.0, 90.0, 0.0], scale: 1.5 },
        Message::Prop { id: u.clone(), model: "mdl/x.rmdl".into(), origin: [0.0; 3], angles: [0.0; 3], scale: 0.001 },
        Message::Move { id: u.clone(), origin: [-LIMIT, 0.0, LIMIT], angles: [0.0; 3] },
        Message::Delete { id: u.clone() },
        Message::Reload { stem: "mp_rr_gauntlet".into() },
        Message::Status,
    ];
    for m in &good {
        assert!(protocol::check(m).is_ok(), "{m:?}");
    }

    // And everything that must not.
    let bad = [
        Message::Prop { id: "not-a-uuid".into(), model: "mdl/x.rmdl".into(), origin: [0.0; 3], angles: [0.0; 3], scale: 1.0 },
        Message::Prop { id: u.clone(), model: "../../windows/system32/cmd.exe".into(), origin: [0.0; 3], angles: [0.0; 3], scale: 1.0 },
        Message::Prop { id: u.clone(), model: "mdl/x.exe".into(), origin: [0.0; 3], angles: [0.0; 3], scale: 1.0 },
        Message::Prop { id: u.clone(), model: "mdl/x.rmdl".into(), origin: [f32::NAN, 0.0, 0.0], angles: [0.0; 3], scale: 1.0 },
        Message::Prop { id: u.clone(), model: "mdl/x.rmdl".into(), origin: [0.0; 3], angles: [0.0; 3], scale: -1.0 },
        Message::Prop { id: u.clone(), model: "mdl/x.rmdl".into(), origin: [LIMIT + 1.0, 0.0, 0.0], angles: [0.0; 3], scale: 1.0 },
        Message::Prop { id: u.clone(), model: "mdl/x.rmdl; Doomed()".into(), origin: [0.0; 3], angles: [0.0; 3], scale: 1.0 },
        Message::Move { id: String::new(), origin: [0.0; 3], angles: [0.0; 3] },
        Message::Delete { id: "'; ServerCommand(\"quit\") //".into() },
        Message::Reload { stem: "../../etc".into() },
        Message::Reload { stem: "mp_gauntlet; ServerCommand(\"quit\")".into() },
    ];
    for m in &bad {
        let r = protocol::check(m);
        assert!(r.is_err(), "{m:?} should be refused");
    }

    // A uuid is normalised, so the text the server sees is one spelling.
    let up = id().to_uppercase();
    let c = protocol::check(&Message::Delete { id: up.clone() }).expect("valid");
    assert_eq!(c.id(), Some(up.to_lowercase().as_str()));

    // Frames are one known message, and nothing else.
    assert!(protocol::parse_frame(br#"{"op":"prop","id":"x"}"#).is_err());
    assert!(protocol::parse_frame(br#"{"op":"status"}"#).is_ok());
    assert!(protocol::parse_frame(br#"{"op":"eval","code":"1+1"}"#).is_err());
    assert!(protocol::parse_frame(b"[]").is_err());
    assert!(protocol::parse_frame(&vec![b'{'; 100_000]).is_err());
}

#[test]
fn live_script_generation() {
    let u = "11111111-2222-3333-4444-555555555555";
    let prop = protocol::check(&Message::Prop {
        id: u.into(),
        model: "mdl/props/caustic_mug/caustic_mug_v.rmdl".into(),
        origin: [1.0, -2.5, 0.25],
        angles: [0.0, 90.0, 0.0],
        scale: 1.0,
    })
    .expect("valid");
    assert_eq!(
        protocol::script(&prop),
        "ReMap_LiveProp( \"11111111-2222-3333-4444-555555555555\", $\"mdl/props/caustic_mug/caustic_mug_v.rmdl\", < 1, -2.5, 0.25 >, < 0, 90, 0 >, true, 1 );"
    );

    let mv = protocol::check(&Message::Move {
        id: u.into(),
        origin: [10.0, 20.0, 30.0],
        angles: [0.0, 0.0, 0.0],
    })
    .expect("valid");
    assert_eq!(
        protocol::script(&mv),
        "ReMap_LiveMove( \"11111111-2222-3333-4444-555555555555\", < 10, 20, 30 >, < 0, 0, 0 > );"
    );

    let del = protocol::check(&Message::Delete { id: u.into() }).expect("valid");
    assert_eq!(
        protocol::script(&del),
        "ReMap_LiveDelete( \"11111111-2222-3333-4444-555555555555\" );"
    );

    let rl = protocol::check(&Message::Reload { stem: "mp_rr_gauntlet".into() }).expect("valid");
    assert_eq!(protocol::script(&rl), "GameRules_ChangeMap( \"mp_rr_gauntlet\", GetCurrentPlaylistName());");

    let st = protocol::check(&Message::Status).expect("valid");
    assert_eq!(
        protocol::script(&st),
        r#"AgentLink_Reply( "{\"map\":\"" + GetMapName() + "\",\"playlist\":\"" + GetCurrentPlaylistName() + "\"}" );"#
    );

    // The formatting is total: a whole number, a fraction and a negative zero
    // all have one spelling each.
    let zero = protocol::check(&Message::Move { id: u.into(), origin: [-0.0, 0.5, -0.0], angles: [0.0; 3] }).expect("valid");
    assert!(protocol::script(&zero).contains("< 0, 0.5, 0 >"), "{}", protocol::script(&zero));
    let whole = protocol::check(&Message::Move { id: u.into(), origin: [4.0, -8.0, 16.0], angles: [0.0; 3] }).expect("valid");
    assert!(protocol::script(&whole).contains("< 4, -8, 16 >"));

    // The agent link is a line protocol, so every payload is exactly one line
    // with no control character in it. A newline here would be split into
    // several requests by the server and the fragments read as unknown ops.
    for m in [prop, mv, del, rl, st] {
        let s = protocol::script(&m);
        assert_eq!(s.lines().count(), 1, "{s}");
        assert!(!s.chars().any(char::is_control), "{s:?}");
        assert!(s.len() < 990, "{}", s.len());
    }
}

/// A stand-in for the server's agent socket: the greeting, the token check, and
/// an echo of every line it was sent. It counts the lines it had to refuse, so
/// a payload that arrives split in two is visible rather than silent.
struct FakeAgent {
    port: u16,
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    refused: std::sync::Arc<std::sync::Mutex<usize>>,
}

async fn fake_agent(tag: &str, expect_token: &str) -> FakeAgent {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let refused = std::sync::Arc::new(std::sync::Mutex::new(0));
    let sink = seen.clone();
    let nref = refused.clone();
    let token = expect_token.to_string();
    tokio::spawn(async move {
        while let Ok((sock, _)) = l.accept().await {
            let token = token.clone();
            let sink = sink.clone();
            let nref = nref.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"AGENTLINK 2 server\n").await.ok();
                let mut line = String::new();
                if r.read_line(&mut line).await.is_err() {
                    return;
                }
                let given = line.trim().strip_prefix("AUTH ").unwrap_or("").to_string();
                if given != token {
                    w.write_all(b"ERR bad token\n").await.ok();
                    return;
                }
                w.write_all(b"OK\n").await.ok();
                loop {
                    let mut l = String::new();
                    match r.read_line(&mut l).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let t = l.trim_end().to_string();
                    if t == "PING" {
                        sink.lock().expect("sink").push(t);
                        w.write_all(b"OK queued 0 drained 0\n").await.ok();
                        continue;
                    }
                    if !t.contains(' ') {
                        *nref.lock().expect("refused") += 1;
                        w.write_all(b"ERR unknown op\n").await.ok();
                        continue;
                    }
                    sink.lock().expect("sink").push(t.clone());
                    let reply = if t.starts_with("SCRIPT ") {
                        "OK\n".to_string()
                    } else if t.starts_with("EVAL ") {
                        let id = t.split(' ').nth(1).unwrap_or("").to_string();
                        format!("RESULT {id} ok {{\"map\":\"mp_test\"}}\n")
                    } else {
                        *nref.lock().expect("refused") += 1;
                        "ERR unknown op\n".to_string()
                    };
                    if w.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
        let _ = tag;
    });
    FakeAgent { port, seen, refused }
}

#[tokio::test(flavor = "multi_thread")]
async fn live_fake_agent_roundtrip() {
    let dir = std::env::temp_dir().join("r5ed-live-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("platform")).expect("platform");
    let token: String = std::iter::repeat_n('a', 64).collect();
    std::fs::write(dir.join("platform").join("agentlink.token"), &token).expect("token file");

    let agent = fake_agent("roundtrip", &token).await;
    let target = Target { game_root: dir.clone(), port: agent.port };
    let link = Link::with_timeout(target.clone(), Duration::from_secs(5));

    // The target reads the token the server wrote, and refuses a bad one.
    assert_eq!(target.token().expect("token"), token);
    let bad = Target { game_root: dir.join("nope"), port: agent.port };
    assert!(bad.token().is_err());
    std::fs::write(dir.join("platform").join("agentlink.token"), "short").expect("write");
    assert!(target.token().is_err());
    std::fs::write(dir.join("platform").join("agentlink.token"), &token).expect("write");

    // A wrong token is refused by the socket.
    std::fs::write(dir.join("platform").join("agentlink.token"), "b".repeat(64)).expect("write");
    let wrong = Link::with_timeout(Target { game_root: dir.clone(), port: agent.port }, Duration::from_secs(5));
    assert!(matches!(wrong.session().await, Err(r5ed_host::ApiError::Forbidden)));
    std::fs::write(dir.join("platform").join("agentlink.token"), &token).expect("write");

    // The right one gets a session and a greeting.
    let mut s = link.session().await.expect("session");
    assert_eq!(s.greeting(), "AGENTLINK 2 server");

    // A status query goes out as EVAL and comes back as the answer body.
    let status_code = protocol::script(&protocol::check(&Message::Status).expect("valid"));
    let body = s.eval("r5ed1", "server", &status_code).await.expect("eval");
    assert_eq!(body, "{\"map\":\"mp_test\"}");

    // A prop goes out as one SCRIPT line, and the line is byte for byte the
    // text the generator produced.
    let u = id();
    let prop = Message::Prop {
        id: u,
        model: "mdl/props/caustic_mug/caustic_mug_v.rmdl".into(),
        origin: [1.0, 2.0, 3.0],
        angles: [0.0, 0.0, 0.0],
        scale: 1.0,
    };
    let handled = r5ed_host::live::handle(&link, serde_json::to_string(&prop).expect("json").as_bytes()).await;
    assert_eq!(handled.expect("sent"), "");
    let expected = format!("SCRIPT {}", protocol::script(&protocol::check(&prop).expect("valid")));
    let seen = agent.seen.lock().expect("seen").clone();
    assert!(seen.contains(&expected), "expected {expected:?} in {seen:?}");

    // Every operation makes it over the wire, and an invalid one never does.
    for m in [
        Message::Move { id: id(), origin: [1.0, 1.0, 1.0], angles: [0.0; 3] },
        Message::Delete { id: id() },
        Message::Reload { stem: "mp_test".into() },
    ] {
        let j = serde_json::to_string(&m).expect("json");
        r5ed_host::live::handle(&link, j.as_bytes()).await.expect("sent");
    }
    let before = agent.seen.lock().expect("seen").len();
    assert!(r5ed_host::live::handle(&link, br#"{"op":"delete","id":"nope"}"#).await.is_err());
    assert!(r5ed_host::live::handle(&link, br#"{"op":"exec"}"#).await.is_err());
    assert_eq!(agent.seen.lock().expect("seen").len(), before, "a refused message sends nothing");

    // Not one line was refused, which is the proof that every payload really
    // did arrive as a single line and not as fragments the server cannot parse.
    assert_eq!(*agent.refused.lock().expect("refused"), 0);

    // And the map change used the verb the server's own script tree uses.
    let seen = agent.seen.lock().expect("seen").clone();
    assert!(
        seen.iter().any(|l| l.contains(protocol::CHANGE_MAP) && l.contains("mp_test")),
        "{seen:?}"
    );

    // A dead port is an error, not a hang.
    let dead = Link::with_timeout(Target { game_root: dir.clone(), port: 1 }, Duration::from_millis(300));
    assert!(dead.session().await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
