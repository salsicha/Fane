//! The HTTP side of Fane's model server: an OpenAI-compatible API that sends each request
//! where `[routes]` says (a model on this computer, another computer running Fane, or another
//! server), plus a video endpoint and a status endpoint.
//!
//! Requests from this computer need nothing; with sharing on, requests from other computers
//! must carry `Authorization: Bearer <token>`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use futures::StreamExt;
use serde_json::{Value, json};

use super::arbiter::Lease;
use super::{RtError, Runtime, catalog};
use crate::config::Role;
use crate::llm::find_installed;

type Rt = Arc<Runtime>;

/// Listen on the configured port: this computer only, or every interface when sharing is on.
pub async fn bind(cfg: &crate::config::RuntimeCfg) -> Result<tokio::net::TcpListener, String> {
    let host = if cfg.share { "0.0.0.0" } else { "127.0.0.1" };
    let addr = format!("{host}:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| format!("can't listen on {addr}: {e}"))?;
    tracing::info!("Fane's model server is listening on {addr}");
    Ok(listener)
}

/// Serve on `listener` until `shutdown` resolves.
pub async fn run(
    listener: tokio::net::TcpListener,
    rt: Rt,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    axum::serve(listener, app(rt).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| e.to_string())
}

/// Whether a Fane model server already answers at `url` (e.g. the background service).
pub async fn answering(url: &str) -> bool {
    let c = crate::llm::http_client(Duration::from_secs(2));
    match c.get(format!("{url}/fane/v1/info")).send().await {
        Ok(r) => r.json::<Value>().await.is_ok_and(|v| v.get("fane").is_some()),
        Err(_) => false,
    }
}

pub fn app(rt: Rt) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/fane/v1/info", get(info))
        .route("/fane/v1/unload", post(unload))
        .route("/fane/v1/reload", post(reload))
        .route("/fane/v1/videos", post(videos))
        .route("/v1/models", get(models))
        .route("/v1/images/generations", post(images))
        .route("/v1/{*rest}", any(chat))
        .with_state(rt)
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (status, axum::Json(json!({"error": {"message": msg, "type": "fane"}}))).into_response()
}

/// Requests from other computers must carry the sharing token (and sharing must be on).
fn authorized(rt: &Runtime, peer: &SocketAddr, headers: &HeaderMap) -> bool {
    if peer.ip().is_loopback() {
        return true;
    }
    let cfg = rt.cfg().runtime;
    if !cfg.share || cfg.token.is_empty() {
        return false;
    }
    let sent = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    let sent = sent.strip_prefix("Bearer ").unwrap_or(sent).trim();
    constant_time_eq(sent.as_bytes(), cfg.token.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

macro_rules! guard {
    ($rt:expr, $peer:expr, $headers:expr) => {
        if !authorized(&$rt, &$peer, &$headers) {
            return error(StatusCode::UNAUTHORIZED, "this Fane needs its sharing token");
        }
    };
}

async fn info(State(rt): State<Rt>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    // Other computers without the token only learn that this is Fane (so discovery can list it).
    if !authorized(&rt, &peer, &headers) {
        return axum::Json(json!({"fane": env!("CARGO_PKG_VERSION"), "name": super::hostname(), "locked": true}))
            .into_response();
    }
    axum::Json(rt.info()).into_response()
}

async fn unload(
    State(rt): State<Rt>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    guard!(rt, peer, headers);
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    rt.unload(v["model"].as_str().unwrap_or("")).await;
    axum::Json(json!({"ok": true})).into_response()
}

/// Re-read the config file (after Settings or `fane` commands changed it).
async fn reload(State(rt): State<Rt>, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> Response {
    if !peer.ip().is_loopback() {
        return error(StatusCode::FORBIDDEN, "only from this computer");
    }
    match crate::config::Config::load() {
        Ok(cfg) => {
            rt.set_cfg(cfg);
            rt.redetect();
            axum::Json(json!({"ok": true})).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

async fn models(State(rt): State<Rt>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    guard!(rt, peer, headers);
    let cfg = rt.cfg();
    let mut data: Vec<Value> = vec![];
    let mut seen = std::collections::HashSet::new();
    // This computer's models, if a chat or code route uses it.
    let routed_here = |m: &str| cfg.routes.for_role(rt.role_of(m)).iter().any(|t| t == "local");
    let local_ok = |m: &str| routed_here(m) && rt.can_run_llm(m, rt.role_of(m));
    for m in catalog::available_llms(&cfg.runtime) {
        if local_ok(&m) && seen.insert(m.clone()) {
            data.push(json!({"id": m, "object": "model", "owned_by": "local"}));
        }
    }
    // Models other computers serve, if they're in a chat or code route.
    let mut remotes: Vec<&String> =
        cfg.routes.chat.iter().chain(&cfg.routes.code).filter(|t| cfg.remotes.contains_key(*t)).collect();
    remotes.dedup();
    for name in remotes {
        if let Ok((kind, list)) = rt.remote_info(name).await
            && kind != "sd"
        {
            for m in list {
                if seen.insert(m.clone()) {
                    data.push(json!({"id": m, "object": "model", "owned_by": name}));
                }
            }
        }
    }
    axum::Json(json!({"object": "list", "data": data})).into_response()
}

/// The name a remote knows `model` by (Ollama may call qwen3.8:27b "qwen3.8:latest").
fn remote_name(model: &str, available: &[String]) -> Option<String> {
    let mut names = vec![model.to_string()];
    if let Some(spec) = catalog::llm_spec(model) {
        names.push(spec.name.to_string());
        names.extend(spec.ollama.iter().map(|s| s.to_string()));
    }
    names.iter().find_map(|n| find_installed(n, available))
}

/// Chat, completions, embeddings…: route by the request's model.
async fn chat(
    State(rt): State<Rt>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    guard!(rt, peer, headers);
    if method != Method::POST {
        return error(StatusCode::NOT_FOUND, format!("{} isn't supported", uri.path()));
    }
    let mut req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad JSON: {e}")),
    };
    let wanted = req["model"].as_str().unwrap_or("").to_string();
    let model = if wanted.is_empty() { rt.model_for(Role::Chat) } else { wanted };
    let role = rt.role_of(&model);
    let cfg = rt.cfg();
    let path = uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| uri.path().to_string());
    let mut tried: Vec<String> = vec![];
    for target in cfg.routes.for_role(role) {
        match target.as_str() {
            "local" => match rt.ensure_llm(&model, role).await {
                Ok((port, lease)) => {
                    let url = format!("http://127.0.0.1:{port}{path}");
                    return forward(&rt, &url, None, body.clone(), Some(lease)).await;
                }
                Err(RtError::NotHere(why)) => tried.push(format!("this computer: {why}")),
                Err(RtError::Failed(why)) => tried.push(format!("this computer: {why}")),
            },
            "claude" | "codex" => {} // handled by the coding tool, not here
            name => {
                let Some(remote) = cfg.remotes.get(name) else {
                    tried.push(format!("{name}: not in [remotes]"));
                    continue;
                };
                match rt.remote_info(name).await {
                    Ok((kind, list)) if kind != "sd" => match remote_name(&model, &list) {
                        Some(theirs) => {
                            req["model"] = json!(theirs);
                            let url = format!("{}{path}", remote.url.trim_end_matches('/'));
                            let body = Bytes::from(serde_json::to_vec(&req).unwrap_or_default());
                            return forward(&rt, &url, Some(&remote.token), body, None).await;
                        }
                        None => tried.push(format!("{name}: doesn't have {model}")),
                    },
                    Ok(_) => tried.push(format!("{name}: only makes images")),
                    Err(e) => tried.push(format!("{name}: {e}")),
                }
            }
        }
    }
    error(StatusCode::SERVICE_UNAVAILABLE, format!("nowhere to run {model} ({})", tried.join("; ")))
}

/// Pass a request on and stream the answer back, holding `lease` until it's done.
async fn forward(rt: &Runtime, url: &str, token: Option<&str>, body: Bytes, lease: Option<Lease>) -> Response {
    let mut req = rt.client.post(url).header(header::CONTENT_TYPE, "application/json").body(body);
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let ctype = resp.headers().get(header::CONTENT_TYPE).cloned();
            let stream = resp.bytes_stream().map(move |chunk| {
                let _hold = &lease;
                chunk
            });
            let mut out = Response::new(Body::from_stream(stream));
            *out.status_mut() = status;
            if let Some(c) = ctype {
                out.headers_mut().insert(header::CONTENT_TYPE, c);
            }
            out
        }
        Err(e) => error(StatusCode::BAD_GATEWAY, format!("{url}: {e}")),
    }
}

async fn images(
    State(rt): State<Rt>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    guard!(rt, peer, headers);
    media(&rt, Role::Image, body).await
}

async fn videos(
    State(rt): State<Rt>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    guard!(rt, peer, headers);
    media(&rt, Role::Video, body).await
}

/// An image or video request, wherever `[routes]` sends it.
async fn media(rt: &Rt, role: Role, body: Bytes) -> Response {
    let cfg = rt.cfg();
    let req: Value = serde_json::from_slice(&body).unwrap_or_default();
    let mut tried: Vec<String> = vec![];
    for target in cfg.routes.for_role(role) {
        match target.as_str() {
            "local" => match rt.ensure_sd(role).await {
                Ok((port, lease, _)) => {
                    let base = format!("http://127.0.0.1:{port}");
                    return match role {
                        Role::Video => video_job(rt, &base, None, &req, Some(lease)).await,
                        _ => {
                            forward(rt, &format!("{base}/v1/images/generations"), None, body.clone(), Some(lease)).await
                        }
                    };
                }
                Err(e) => tried.push(format!("this computer: {e}")),
            },
            "claude" | "codex" => {}
            name => {
                let Some(remote) = cfg.remotes.get(name) else { continue };
                let base = remote.url.trim_end_matches('/').to_string();
                match rt.remote_info(name).await {
                    Ok((kind, _)) if kind == "fane" => {
                        let path = if role == Role::Video { "/fane/v1/videos" } else { "/v1/images/generations" };
                        return forward(rt, &format!("{base}{path}"), Some(&remote.token), body.clone(), None).await;
                    }
                    Ok((kind, _)) if kind == "sd" => {
                        if let Err(e) = load_remote_sd(rt, &base, &remote.token, role).await {
                            tried.push(format!("{name}: {e}"));
                            continue;
                        }
                        return match role {
                            Role::Video => video_job(rt, &base, Some(&remote.token), &req, None).await,
                            _ => {
                                forward(
                                    rt,
                                    &format!("{base}/v1/images/generations"),
                                    Some(&remote.token),
                                    body.clone(),
                                    None,
                                )
                                .await
                            }
                        };
                    }
                    Ok(_) => tried.push(format!("{name}: doesn't make {}s", role.as_str())),
                    Err(e) => tried.push(format!("{name}: {e}")),
                }
            }
        }
    }
    error(StatusCode::SERVICE_UNAVAILABLE, format!("nowhere to make {}s ({})", role.as_str(), tried.join("; ")))
}

/// On a plain stable-diffusion.cpp server: load our image/video model if its files are there
/// and something else is loaded (or nothing).
async fn load_remote_sd(rt: &Runtime, base: &str, token: &str, role: Role) -> Result<(), String> {
    let get = |path: &str| {
        let mut r = rt.client.get(format!("{base}{path}")).timeout(Duration::from_secs(20));
        if !token.is_empty() {
            r = r.bearer_auth(token);
        }
        r
    };
    let status: Value = get("/sdcpp/v1/models/status")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let caps: Value =
        get("/sdcpp/v1/capabilities").send().await.map_err(|e| e.to_string())?.json().await.unwrap_or_default();
    let mode = if role == Role::Video { "vid_gen" } else { "img_gen" };
    let ready = status["state"] == "ready";
    let supports = caps["supported_modes"].as_array().is_some_and(|m| m.iter().any(|x| x == mode));
    if ready && supports {
        return Ok(()); // whatever it has loaded can do this
    }
    let spec = catalog::sd_spec(&rt.model_for(role)).ok_or("no model configured for this")?;
    let files: Value = get("/sdcpp/v1/model-files")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let list = files["files"].as_array().cloned().unwrap_or_default();
    let mut selection = serde_json::Map::new();
    for (slot, _, reference) in spec.files {
        let file = reference.rsplit('/').next().unwrap_or(reference);
        let id = list
            .iter()
            .find(|f| f["relative_path"].as_str().is_some_and(|p| p.ends_with(file)))
            .and_then(|f| f["id"].as_str())
            .ok_or_else(|| format!("it doesn't have {file}"))?;
        selection.insert(slot.to_string(), json!(id));
    }
    let mut req = rt.client.post(format!("{base}/sdcpp/v1/models/load")).timeout(Duration::from_secs(900));
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    let resp =
        req.json(&json!({"recipe": spec.recipe, "models": selection})).send().await.map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("loading {} failed: {}", spec.label, resp.text().await.unwrap_or_default()))
    }
}

/// Run a video job on a stable-diffusion.cpp server and wait for the file.
async fn video_job(rt: &Runtime, base: &str, token: Option<&str>, req: &Value, lease: Option<Lease>) -> Response {
    let _hold = lease;
    let seconds = req["seconds"].as_f64().unwrap_or(2.0).clamp(0.5, 10.0);
    let fps = req["fps"].as_u64().unwrap_or(16);
    let (w, h) = parse_size(req["size"].as_str().unwrap_or("832x480")).unwrap_or((832, 480));
    let mut job = json!({
        "prompt": req["prompt"].as_str().unwrap_or(""),
        "negative_prompt": req["negative_prompt"].as_str().unwrap_or(""),
        "width": w, "height": h,
        "video_frames": (seconds * fps as f64) as u64 + 1,
        "fps": fps,
        "seed": req["seed"].as_i64().unwrap_or(-1),
        "output_format": "webm",
    });
    if let Some(img) = req["image"].as_str().filter(|s| !s.is_empty()) {
        job["init_image"] = json!(img);
    }
    let auth = |r: reqwest::RequestBuilder| match token.filter(|t| !t.is_empty()) {
        Some(t) => r.bearer_auth(t),
        None => r,
    };
    let mut submit = auth(rt.client.post(format!("{base}/sdcpp/v1/vid_gen"))).json(&job).send().await;
    if let Ok(r) = &submit
        && r.status() == reqwest::StatusCode::BAD_REQUEST
    {
        // Built without WebM: animated WebP instead.
        job["output_format"] = json!("webp");
        submit = auth(rt.client.post(format!("{base}/sdcpp/v1/vid_gen"))).json(&job).send().await;
    }
    let accepted: Value = match submit {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        Ok(r) => {
            return error(
                StatusCode::BAD_GATEWAY,
                format!("video request refused: {}", r.text().await.unwrap_or_default()),
            );
        }
        Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    let Some(id) = accepted["id"].as_str() else { return error(StatusCode::BAD_GATEWAY, "no job id") };
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let poll = auth(rt.client.get(format!("{base}/sdcpp/v1/jobs/{id}"))).send().await;
        let Ok(r) = poll else { continue };
        let v: Value = r.json().await.unwrap_or_default();
        match v["status"].as_str().unwrap_or("") {
            "completed" => {
                let res = &v["result"];
                return axum::Json(json!({
                    "b64_json": res["b64_json"], "mime_type": res["mime_type"], "output_format": res["output_format"],
                    "fps": res["fps"], "frame_count": res["frame_count"],
                }))
                .into_response();
            }
            "failed" | "cancelled" => {
                return error(StatusCode::BAD_GATEWAY, format!("video generation failed: {}", v["error"]));
            }
            _ if started.elapsed() > Duration::from_secs(3600) => {
                return error(StatusCode::GATEWAY_TIMEOUT, "video generation took over an hour");
            }
            _ => {}
        }
    }
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RemoteCfg};

    /// A stand-in for another computer's server: lists qwen3.8 under Ollama's name and echoes
    /// what it was asked (model and token).
    async fn fake_remote() -> String {
        let app = Router::new()
            .route("/fane/v1/info", get(|| async { axum::Json(json!({"fane": "0.0", "name": "nuc"})) }))
            .route(
                "/v1/models",
                get(|| async { axum::Json(json!({"data": [{"id": "qwen3.8:latest"}]})) }),
            )
            .route(
                "/v1/chat/completions",
                post(|headers: HeaderMap, body: Bytes| async move {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
                    axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": format!("{}|{auth}", v["model"].as_str().unwrap())}}]}))
                }),
            )
            .route("/v1/images/generations", post(|| async { axum::Json(json!({"data": [{"b64_json": "aW1n"}]})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    async fn router(cfg: Config) -> String {
        let rt = Runtime::new(cfg);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { run(listener, rt, std::future::pending()).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn routes_to_the_next_target_with_its_token_and_name() {
        let remote = fake_remote().await;
        let mut cfg = Config::default();
        let home = std::env::temp_dir().join(format!("fane-router-{}", std::process::id()));
        cfg.runtime.home = home.display().to_string();
        cfg.runtime.reuse_ollama = false; // nothing on "this computer"
        cfg.remotes.insert("nuc".into(), RemoteCfg { url: remote, token: "s3cret".into(), kind: String::new() });
        cfg.routes.code = vec!["local".into(), "nuc".into(), "claude".into()];
        cfg.routes.image = vec!["nuc".into()];
        let url = router(cfg).await;
        let c = reqwest::Client::new();
        let ask = |model: &str| {
            c.post(format!("{url}/v1/chat/completions"))
                .json(&json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}))
                .send()
        };
        // The coding model isn't here, so it goes to the NUC, under the NUC's name for it.
        let r = ask("qwen3.8:27b").await.unwrap();
        assert_eq!(r.status(), 200);
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "qwen3.8:latest|Bearer s3cret");
        // Chat only routes to "local", which can't run it.
        let r = ask("gemma4:26b").await.unwrap();
        assert_eq!(r.status(), 503);
        let v: Value = r.json().await.unwrap();
        assert!(v["error"]["message"].as_str().unwrap().contains("nowhere to run gemma4:26b"), "{v}");
        // The model list includes what the NUC has.
        let v: Value = c.get(format!("{url}/v1/models")).send().await.unwrap().json().await.unwrap();
        assert!(v["data"].as_array().unwrap().iter().any(|m| m["id"] == "qwen3.8:latest" && m["owned_by"] == "nuc"));
        // Images go to the NUC as is.
        let v: Value = c
            .post(format!("{url}/v1/images/generations"))
            .json(&json!({"prompt": "a cat"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["data"][0]["b64_json"], "aW1n");
        // Videos: no route.
        let r = c.post(format!("{url}/fane/v1/videos")).json(&json!({"prompt": "x"})).send().await.unwrap();
        assert_eq!(r.status(), 503);
    }

    #[tokio::test]
    async fn other_computers_need_the_token() {
        let mut cfg = Config::default();
        let rt = Runtime::new(cfg.clone());
        let lan: SocketAddr = "192.168.1.9:5000".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        assert!(authorized(&rt, &local, &h), "this computer needs nothing");
        assert!(!authorized(&rt, &lan, &h), "sharing is off");
        cfg.runtime.share = true;
        cfg.runtime.token = "t0ken".into();
        rt.set_cfg(cfg);
        assert!(!authorized(&rt, &lan, &h));
        h.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(!authorized(&rt, &lan, &h));
        h.insert(header::AUTHORIZATION, "Bearer t0ken".parse().unwrap());
        assert!(authorized(&rt, &lan, &h));
    }

    #[test]
    fn remote_names_and_sizes() {
        let have = vec!["qwen3.8:latest".to_string(), "gemma4:26b".to_string()];
        assert_eq!(remote_name("qwen3.8:27b", &have).as_deref(), Some("qwen3.8:latest"));
        assert_eq!(remote_name("gemma4:26b", &have).as_deref(), Some("gemma4:26b"));
        assert_eq!(remote_name("gpt-oss:20b", &have), None);
        assert_eq!(parse_size("1024x768"), Some((1024, 768)));
        assert!(
            constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd") && !constant_time_eq(b"a", b"ab")
        );
    }
}
