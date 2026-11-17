use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};

use super::*;
use crate::github::transport::{Reqwest, Token};
use crate::github::{Req, Transport};

const EXPIRES: &str = "2026-11-16T13:00:00Z";
const START: u64 = 1_794_834_000 - 3600; // one hour before EXPIRES

struct Keys {
    pem: String,
    public_der: Vec<u8>,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        Keys {
            pem: private.to_pkcs1_pem(LineEnding::LF).unwrap().to_string(),
            public_der: RsaPublicKey::from(&private)
                .to_pkcs1_der()
                .unwrap()
                .as_bytes()
                .to_vec(),
        }
    })
}

#[derive(Default)]
struct Fake {
    mints: AtomicUsize,
    pings: AtomicUsize,
    discoveries: AtomicUsize,
    jwts: StdMutex<Vec<String>>,
    pinged_with: StdMutex<Vec<String>>,
    /// Statuses the mint answers with, front first; 201 once empty.
    mint_script: StdMutex<Vec<u16>>,
    /// Bearer tokens `/ping` refuses.
    revoked: StdMutex<Vec<String>>,
    ping_always_401: std::sync::atomic::AtomicBool,
}

fn bearer(h: &HeaderMap) -> String {
    h["authorization"]
        .to_str()
        .unwrap()
        .trim_start_matches("Bearer ")
        .to_owned()
}

async fn serve(fake: Arc<Fake>) -> String {
    async fn mint(State(f): State<Arc<Fake>>, h: HeaderMap) -> (StatusCode, axum::Json<Value>) {
        let n = f.mints.fetch_add(1, Ordering::SeqCst) + 1;
        f.jwts.lock().unwrap().push(bearer(&h));
        let scripted = {
            let mut s = f.mint_script.lock().unwrap();
            (!s.is_empty()).then(|| s.remove(0))
        };
        match scripted {
            Some(code) if code != 201 => (
                StatusCode::from_u16(code).unwrap(),
                axum::Json(json!({
                    "message": format!("bad credentials, saw ghs_{}", "A".repeat(36))
                })),
            ),
            _ => (
                StatusCode::CREATED,
                axum::Json(json!({ "token": format!("ghs_faketoken{n}"), "expires_at": EXPIRES })),
            ),
        }
    }
    async fn discover(State(f): State<Arc<Fake>>) -> axum::Json<Value> {
        f.discoveries.fetch_add(1, Ordering::SeqCst);
        axum::Json(json!({ "id": 77 }))
    }
    async fn ping(State(f): State<Arc<Fake>>, h: HeaderMap) -> (StatusCode, axum::Json<Value>) {
        f.pings.fetch_add(1, Ordering::SeqCst);
        let token = bearer(&h);
        let refused =
            f.ping_always_401.load(Ordering::SeqCst) || f.revoked.lock().unwrap().contains(&token);
        f.pinged_with.lock().unwrap().push(token);
        let code = if refused {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::OK
        };
        (code, axum::Json(json!({ "ok": !refused })))
    }
    let app = Router::new()
        .route("/app/installations/{id}/access_tokens", post(mint))
        .route("/repos/{owner}/{repo}/installation", get(discover))
        .route("/orgs/{org}/installation", get(discover))
        .route("/ping", get(ping))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

struct Rig {
    fake: Arc<Fake>,
    api: String,
    now: Arc<AtomicU64>,
    logs: Arc<StdMutex<Vec<String>>>,
    dir: std::path::PathBuf,
}

impl Rig {
    async fn new(name: &str) -> Self {
        let fake = Arc::new(Fake::default());
        let dir = std::env::temp_dir().join(format!("kbl-app-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("key.pem"), &keys().pem).unwrap();
        Self {
            api: serve(fake.clone()).await,
            fake,
            now: Arc::new(AtomicU64::new(START)),
            logs: Arc::default(),
            dir,
        }
    }

    fn auth(&self, installation: Installation) -> AppAuth {
        let now = self.now.clone();
        let logs = self.logs.clone();
        AppAuth::new(
            &AppConfig {
                id: "4242".into(),
                installation,
                private_key_file: self.dir.join("key.pem"),
            },
            &self.api,
            Arc::new(move || now.load(Ordering::SeqCst)),
            Arc::new(move |m| logs.lock().unwrap().push(m.to_owned())),
        )
        .unwrap()
    }

    fn at(&self, t: u64) {
        self.now.store(t, Ordering::SeqCst);
    }

    fn mints(&self) -> usize {
        self.fake.mints.load(Ordering::SeqCst)
    }
}

fn b64(s: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(s).unwrap()
}

#[test]
fn the_start_is_one_hour_before_the_expiry() {
    assert_eq!(
        super::super::parse_utc_ms(EXPIRES).unwrap() / 1000,
        START + 3600
    );
}

#[tokio::test]
async fn the_jwt_carries_the_claims_and_verifies_with_the_public_key() {
    let rig = Rig::new("jwt").await;
    rig.auth(Installation::Id(9)).token().await.unwrap();
    let jwt = rig.fake.jwts.lock().unwrap()[0].clone();
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let head: Value = serde_json::from_slice(&b64(parts[0])).unwrap();
    assert_eq!(head, json!({ "alg": "RS256", "typ": "JWT" }));
    let claims: Value = serde_json::from_slice(&b64(parts[1])).unwrap();
    let (iat, exp) = (
        claims["iat"].as_u64().unwrap(),
        claims["exp"].as_u64().unwrap(),
    );
    assert_eq!(iat, START - 60, "issued 60s in the past for clock skew");
    assert!(exp > iat && exp - iat <= 540, "valid at most nine minutes");
    assert!(
        exp <= START + 600,
        "inside GitHub's ten minute limit from now"
    );
    assert_eq!(claims["iss"], "4242");
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &keys().public_der)
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &b64(parts[2]),
        )
        .expect("the signature verifies");
}

#[tokio::test]
async fn a_token_is_minted_once_and_reused() {
    let rig = Rig::new("reuse").await;
    let auth = rig.auth(Installation::Id(9));
    let a = auth.token().await.unwrap();
    rig.at(START + 1800);
    let b = auth.token().await.unwrap();
    assert_eq!(a, b);
    assert_eq!(rig.mints(), 1);
}

#[tokio::test]
async fn a_token_is_replaced_five_minutes_before_it_expires() {
    let rig = Rig::new("refresh").await;
    let auth = rig.auth(Installation::Id(9));
    let a = auth.token().await.unwrap();
    rig.at(START + 3600 - 301);
    assert_eq!(auth.token().await.unwrap(), a, "301s left is still fine");
    rig.at(START + 3600 - 300);
    let b = auth.token().await.unwrap();
    assert_ne!(a, b);
    assert_eq!(rig.mints(), 2);
}

#[tokio::test]
async fn the_installation_is_found_from_a_repo_once() {
    let rig = Rig::new("discover").await;
    let auth = rig.auth(Installation::Repo("GetFelix/felix".into()));
    auth.token().await.unwrap();
    rig.at(START + 3600 - 100);
    auth.token().await.unwrap();
    assert_eq!(rig.mints(), 2);
    assert_eq!(rig.fake.discoveries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_request_refreshes_once_and_retries() {
    let rig = Rig::new("retry").await;
    let api = Reqwest::new(Token::App(Box::new(rig.auth(Installation::Id(9))))).unwrap();
    let url = format!("{}/ping", rig.api);
    assert_eq!(api.send(Req::get(url.clone())).await.unwrap().status, 200);
    rig.fake
        .revoked
        .lock()
        .unwrap()
        .push("ghs_faketoken1".into());
    let res = api.send(Req::get(url)).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(rig.mints(), 2);
    assert_eq!(
        *rig.fake.pinged_with.lock().unwrap(),
        ["ghs_faketoken1", "ghs_faketoken1", "ghs_faketoken2"]
    );
}

#[tokio::test]
async fn a_second_refusal_fails_clearly_after_one_refresh() {
    let rig = Rig::new("fail").await;
    rig.fake.ping_always_401.store(true, Ordering::SeqCst);
    let api = Reqwest::new(Token::App(Box::new(rig.auth(Installation::Id(9))))).unwrap();
    let err = format!(
        "{:#}",
        api.send(Req::get(format!("{}/ping", rig.api)))
            .await
            .unwrap_err()
    );
    assert!(
        err.contains("401") && err.contains("freshly minted"),
        "{err}"
    );
    assert_eq!(rig.mints(), 2, "one refresh, not a loop");
    assert_eq!(rig.fake.pings.load(Ordering::SeqCst), 2);
    assert!(!err.contains("ghs_"), "{err}");
}

#[tokio::test]
async fn a_token_file_is_read_as_before_and_never_refreshed() {
    let rig = Rig::new("file").await;
    let file = rig.dir.join("token");
    std::fs::write(&file, "ghs_fromfile\n").unwrap();
    let api = Reqwest::new(Token::File(file.clone())).unwrap();
    let url = format!("{}/ping", rig.api);
    api.send(Req::get(url.clone())).await.unwrap();
    std::fs::write(&file, "ghs_rotated\n").unwrap();
    rig.fake.revoked.lock().unwrap().push("ghs_rotated".into());
    let res = api.send(Req::get(url)).await.unwrap();
    assert_eq!(res.status, 401, "a 401 is handed back, not retried");
    assert_eq!(
        *rig.fake.pinged_with.lock().unwrap(),
        ["ghs_fromfile", "ghs_rotated"]
    );
    assert_eq!(rig.mints(), 0);
}

#[tokio::test]
async fn no_secret_reaches_a_log_or_an_error() {
    let rig = Rig::new("secrets").await;
    let auth = rig.auth(Installation::Id(9));
    // A mint GitHub refuses, with a token-shaped string in its message.
    rig.fake.mint_script.lock().unwrap().push(401);
    let refused = format!("{:#}", auth.token().await.unwrap_err());
    assert!(refused.contains("401"), "{refused}");
    let token = auth.token().await.unwrap();
    let jwt = rig.fake.jwts.lock().unwrap()[0].clone();
    let body: String = keys()
        .pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();

    let mut seen = vec![
        refused,
        format!("{auth:?}"),
        rig.logs.lock().unwrap().join("\n"),
    ];
    // A key that does not load.
    std::fs::write(
        rig.dir.join("bad.pem"),
        format!(
            "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----\n",
            &body[..200]
        ),
    )
    .unwrap();
    let bad = AppAuth::new(
        &AppConfig {
            id: "1".into(),
            installation: Installation::Id(1),
            private_key_file: rig.dir.join("bad.pem"),
        },
        &rig.api,
        system_clock(),
        stderr_log(),
    )
    .unwrap_err();
    seen.push(format!("{bad:#}"));
    assert!(
        rig.logs
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("minted"))
    );
    for text in &seen {
        for secret in [&token, &jwt, &body, &body[..40].to_owned(), &"A".repeat(36)] {
            assert!(!text.contains(secret.as_str()), "{text} leaks a secret");
        }
    }
}
