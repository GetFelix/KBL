use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const ID_TOKEN: &str = "id-token-secret-aaaa";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A Felix token that says it expires at `exp`, with `tag` to tell them apart.
fn felix_token(tag: &str, exp: i64) -> String {
    let b64 = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
    format!(
        "{}.{}.{tag}",
        b64(r#"{"alg":"none"}"#),
        b64(&format!(r#"{{"exp":{exp}}}"#))
    )
}

/// What the stand-in control plane saw and says next.
struct Plane {
    base: String,
    calls: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
}

/// Answers each exchange with `answer(call number, bearer)`. The reply is a
/// status and a body.
async fn control_plane(
    answer: impl Fn(usize, &str) -> (u16, String) + Send + Sync + 'static,
) -> Plane {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (c, s) = (calls.clone(), seen.clone());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break (
                        head,
                        String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string(),
                    );
                }
            };
            let line = head.lines().next().unwrap().to_string();
            let headers: HashMap<_, _> = head
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(": "))
                .map(|(k, v)| (k.to_lowercase(), v.to_string()))
                .collect();
            let bearer = headers
                .get("authorization")
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("")
                .to_string();
            s.lock().unwrap().push((line, bearer.clone(), body));
            let n = c.fetch_add(1, Ordering::SeqCst);
            let (status, reply) = answer(n, &bearer);
            let out = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            sock.write_all(out.as_bytes()).await.unwrap();
        }
    });
    Plane { base, calls, seen }
}

fn id_file(dir: &Path, text: &str, mode: u32) -> PathBuf {
    let path = dir.join("id-token");
    std::fs::write(&path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    path
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn provider(auth: Auth) -> Arc<dyn TokenProvider> {
    match auth {
        Auth::Renewing(p) => p,
        Auth::Fixed(_) => panic!("expected a renewing credential"),
    }
}

fn tmp() -> tempdir::Dir {
    tempdir::Dir::new()
}

mod tempdir {
    use std::path::{Path, PathBuf};

    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    pub(super) struct Dir(PathBuf);

    impl Dir {
        pub(super) fn new() -> Dir {
            let path = std::env::temp_dir().join(format!(
                "kbl-mcp-cred-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn renewing(plane: &Plane, dir: &Path) -> Arc<dyn TokenProvider> {
    let file = id_file(dir, &format!("{ID_TOKEN}\n"), 0o600);
    provider(
        Auth::from_env(
            env(&[
                ("KBL_FELIX_ID_TOKEN_FILE", file.to_str().unwrap()),
                ("KBL_FELIX_CONTROL_PLANE", &plane.base),
            ]),
            "agents",
        )
        .unwrap(),
    )
}

#[test]
fn a_fixed_token_is_still_a_fixed_token() {
    let auth = Auth::from_env(env(&[("KBL_FELIX_TOKEN", "t")]), "agents").unwrap();
    assert!(matches!(auth, Auth::Fixed(t) if t == "t"));
}

#[test]
fn the_sources_do_not_mix_and_one_is_needed() {
    let both = Auth::from_env(
        env(&[("KBL_FELIX_TOKEN", "t"), ("KBL_FELIX_ID_TOKEN_FILE", "/x")]),
        "agents",
    );
    assert!(both.is_err());
    let two = Auth::from_env(
        env(&[
            ("KBL_FELIX_ID_TOKEN_FILE", "/x"),
            ("KBL_FELIX_ID_TOKEN_COMMAND", "y"),
        ]),
        "agents",
    );
    assert!(two.is_err());
    assert!(Auth::from_env(env(&[]), "agents").is_err());
    // An empty variable is unset, as a host leaves it when it has nothing to give.
    assert!(Auth::from_env(env(&[("KBL_FELIX_TOKEN", "")]), "agents").is_err());
}

#[test]
fn renewing_needs_a_control_plane_that_is_safe_for_a_bearer_token() {
    let with = |url: &str| {
        Auth::from_env(
            env(&[
                ("KBL_FELIX_ID_TOKEN_FILE", "/x"),
                ("KBL_FELIX_CONTROL_PLANE", url),
            ]),
            "agents",
        )
        .is_ok()
    };
    assert!(Auth::from_env(env(&[("KBL_FELIX_ID_TOKEN_FILE", "/x")]), "agents").is_err());
    assert!(with("https://felix.example.com"));
    assert!(with("http://127.0.0.1:8443"));
    assert!(with("http://localhost:8443"));
    assert!(!with("http://felix.example.com"));
    assert!(!with("https://user:pw@felix.example.com"));
    assert!(!with("ftp://felix.example.com"));
}

#[tokio::test]
async fn it_trades_the_id_token_for_a_felix_token_for_the_broker() {
    let exp = now() + 3600;
    let plane = control_plane(move |_, _| {
        (
            200,
            json!({ "felix_token": felix_token("one", exp) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let token = renewing(&plane, dir.path()).token().await.unwrap();
    assert_eq!(token, felix_token("one", exp));
    let seen = plane.seen.lock().unwrap();
    assert_eq!(seen[0].0, "POST /v1/tenants/agents/token/exchange HTTP/1.1");
    assert_eq!(seen[0].1, ID_TOKEN);
    assert!(seen[0].2.contains(r#""audience":"felix-broker""#));
}

#[tokio::test]
async fn a_token_is_reused_until_it_is_due_then_replaced() {
    let plane = control_plane(|n, _| {
        // The first token is already due; later ones last.
        let exp = if n == 0 { now() - 10 } else { now() + 3600 };
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), exp) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let provider = renewing(&plane, dir.path());
    assert!(provider.token().await.unwrap().ends_with("t0"));
    // Due, so the next ask renews.
    assert!(provider.token().await.unwrap().ends_with("t1"));
    // Fresh, so it is not asked for again.
    assert!(provider.token().await.unwrap().ends_with("t1"));
    assert_eq!(plane.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_rotated_id_token_file_is_read_again_on_renewal() {
    let plane = control_plane(|n, _| {
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), now() - 10) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let provider = renewing(&plane, dir.path());
    provider.token().await.unwrap();
    id_file(dir.path(), "rotated-id-token\n", 0o600);
    provider.token().await.unwrap();
    let seen = plane.seen.lock().unwrap();
    assert_eq!(seen[0].1, ID_TOKEN);
    assert_eq!(seen[1].1, "rotated-id-token");
}

#[tokio::test]
async fn a_failed_renewal_keeps_the_current_token_until_it_expires() {
    let exp = now() + 6;
    let plane = control_plane(move |n, _| {
        if n == 0 {
            (
                200,
                json!({ "felix_token": felix_token("first", exp) }).to_string(),
            )
        } else {
            (503, "down".into())
        }
    })
    .await;
    let dir = tmp();
    let provider = renewing(&plane, dir.path());
    let first = provider.token().await.unwrap();
    // Past two thirds of its life: due, and the control plane is down.
    tokio::time::sleep(std::time::Duration::from_millis(4300)).await;
    assert_eq!(provider.token().await.unwrap(), first);
    assert!(plane.calls.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn a_revoked_id_token_is_an_error_that_says_so_without_the_secrets() {
    // A hostile or sloppy control plane echoes everything back.
    let plane = control_plane(|_, bearer| {
        (
            401,
            format!("bad token {bearer} felix_token=leaked-felix-secret"),
        )
    })
    .await;
    let dir = tmp();
    let err = renewing(&plane, dir.path()).token().await.unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains(ID_TOKEN), "{text}");
    assert!(!text.contains("leaked-felix-secret"), "{text}");
}

#[tokio::test]
async fn an_unreachable_control_plane_does_not_name_a_secret() {
    let dir = tmp();
    let file = id_file(dir.path(), ID_TOKEN, 0o600);
    let provider = provider(
        Auth::from_env(
            env(&[
                ("KBL_FELIX_ID_TOKEN_FILE", file.to_str().unwrap()),
                ("KBL_FELIX_CONTROL_PLANE", "http://127.0.0.1:1"),
            ]),
            "agents",
        )
        .unwrap(),
    );
    let text = format!("{:#}", provider.token().await.unwrap_err());
    assert!(!text.contains(ID_TOKEN), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn an_id_token_file_others_can_read_is_refused_without_reading_it() {
    let dir = tmp();
    let file = id_file(dir.path(), ID_TOKEN, 0o644);
    let err = read_id_token(&IdSource::File(file)).await.unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("chmod 600"), "{text}");
    assert!(!text.contains(ID_TOKEN), "{text}");
}

#[tokio::test]
async fn a_file_that_is_not_a_token_is_refused_without_echoing_it() {
    let dir = tmp();
    for body in ["", "  \n", "two words", &"x".repeat(MAX_TOKEN_BYTES + 5)] {
        let file = id_file(dir.path(), body, 0o600);
        let err = read_id_token(&IdSource::File(file)).await.unwrap_err();
        assert!(!format!("{err:#}").contains("two words"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_gives_the_id_token_and_its_stderr_is_never_shown() {
    let ok = read_id_token(&IdSource::Command("echo cmd-token".into()))
        .await
        .unwrap();
    assert_eq!(ok, "cmd-token");
    let err = read_id_token(&IdSource::Command(
        "echo cmd-secret >&2; echo cmd-secret; exit 3".into(),
    ))
    .await
    .unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("failed"), "{text}");
    assert!(!text.contains("cmd-secret"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_that_cannot_run_does_not_show_its_command_line() {
    let err = read_id_token(&IdSource::Command("exit 1 # --password hunter2".into()))
        .await
        .unwrap_err();
    assert!(!format!("{err:#}").contains("hunter2"));
}

#[tokio::test]
async fn the_credential_never_prints_in_a_debug_or_display_of_the_provider_error() {
    let plane = control_plane(|_, _| (200, json!({ "nope": 1 }).to_string())).await;
    let dir = tmp();
    let err = renewing(&plane, dir.path()).token().await.unwrap_err();
    assert!(format!("{err:?}").contains("felix_token"));
    assert!(!format!("{err:?}").contains(ID_TOKEN));
}

#[tokio::test]
async fn a_token_the_broker_refused_is_replaced_on_the_next_ask() {
    let plane = control_plane(|n, _| {
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), now() + 3600) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let provider = renewing(&plane, dir.path());
    let first = provider.token().await.unwrap();
    provider.invalidate("some other token");
    assert_eq!(provider.token().await.unwrap(), first);
    provider.invalidate(&first);
    assert!(provider.token().await.unwrap().ends_with("t1"));
}

#[tokio::test]
async fn streams_that_open_at_once_share_one_renewal() {
    let plane = control_plane(|n, _| {
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), now() + 3600) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let provider = renewing(&plane, dir.path());
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let provider = provider.clone();
            tokio::spawn(async move { provider.token().await })
        })
        .collect();
    let mut tokens = Vec::new();
    for t in tasks {
        tokens.push(t.await.unwrap());
    }
    assert!(tokens.iter().all(|t| t.as_ref().unwrap().ends_with("t0")));
    assert_eq!(plane.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_max_age_renews_a_token_that_would_last_longer() {
    let plane = control_plane(|n, _| {
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), now() + 3600) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let file = id_file(dir.path(), ID_TOKEN, 0o600);
    let provider = provider(
        Auth::from_env(
            env(&[
                ("KBL_FELIX_ID_TOKEN_FILE", file.to_str().unwrap()),
                ("KBL_FELIX_CONTROL_PLANE", &plane.base),
                ("KBL_FELIX_TOKEN_MAX_AGE_MS", "200"),
            ]),
            "agents",
        )
        .unwrap(),
    );
    assert!(provider.token().await.unwrap().ends_with("t0"));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(provider.token().await.unwrap().ends_with("t1"));
}

#[tokio::test]
async fn a_renewal_swaps_in_a_connection_that_opened_with_the_new_token() {
    let plane = control_plane(|n, _| {
        (
            200,
            json!({ "felix_token": felix_token(&format!("t{n}"), now() + 3600) }).to_string(),
        )
    })
    .await;
    let dir = tmp();
    let file = id_file(dir.path(), ID_TOKEN, 0o600);
    let Auth::Renewing(renewer) = Auth::from_env(
        env(&[
            ("KBL_FELIX_ID_TOKEN_FILE", file.to_str().unwrap()),
            ("KBL_FELIX_CONTROL_PLANE", &plane.base),
            ("KBL_FELIX_TOKEN_MAX_AGE_MS", "200"),
        ]),
        "agents",
    )
    .unwrap() else {
        panic!("expected a renewing credential");
    };
    // A "connection" here is the token its streams opened with.
    let first = renewer.token().await.unwrap();
    assert!(first.ends_with("t0"));
    let slot = Arc::new(std::sync::RwLock::new(Arc::new(first.clone())));
    let r = renewer.clone();
    let task = tokio::spawn(reconnect_on_renewal(
        renewer.clone(),
        slot.clone(),
        Some(first),
        move || {
            let r = r.clone();
            async move { Ok(r.current().await.unwrap()) }
        },
    ));
    // Nothing else asks for a token: the loop renews on its own.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    task.abort();
    let now_on = slot.read().unwrap().to_string();
    assert!(!now_on.ends_with("t0"), "still on the first token");
    assert!(plane.calls.load(Ordering::SeqCst) >= 3);
}
