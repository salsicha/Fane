//! Local socket between the `fane` CLI and the running app: one JSON request line, one
//! JSON reply line. Unix: a socket file in the private runtime dir. Windows: a named pipe.

use std::io::{BufRead, BufReader, Write};

use interprocess::local_socket::{ListenerOptions, Name, Stream, prelude::*};
use serde_json::{Value, json};

use crate::config::RUNTIME_DIR;

fn name() -> std::io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        // A path inside the 0700 runtime dir, not the abstract namespace, so other local users
        // can't reach it.
        RUNTIME_DIR.join("fane.sock").into_os_string().to_fs_name::<GenericFilePath>()
    }
    #[cfg(not(unix))]
    {
        use interprocess::local_socket::GenericNamespaced;
        format!("fane-{}.sock", crate::config::whoami()).to_ns_name::<GenericNamespaced>()
    }
}

/// Send one command to the running app. `None` if it isn't running.
pub fn send(cmd: &str, args: Value) -> Option<Value> {
    let mut req = json!({"cmd": cmd});
    if let (Some(r), Value::Object(extra)) = (req.as_object_mut(), args) {
        r.extend(extra);
    }
    let stream = Stream::connect(name().ok()?).ok()?;
    let mut conn = BufReader::new(stream);
    conn.get_mut().write_all(format!("{req}\n").as_bytes()).ok()?;
    let mut line = String::new();
    conn.read_line(&mut line).ok()?;
    if line.trim().is_empty() {
        return Some(json!({}));
    }
    serde_json::from_str(&line).ok()
}

pub fn running() -> bool {
    send("ping", json!({})).is_some()
}

/// Serve requests forever; `handle` maps a request to a reply.
pub async fn serve<F, Fut>(handle: F) -> std::io::Result<()>
where
    F: Fn(Value) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Value> + Send,
{
    use interprocess::local_socket::tokio::{Stream as TokioStream, prelude::*};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&*RUNTIME_DIR)?;
        let _ = std::fs::set_permissions(&*RUNTIME_DIR, std::fs::Permissions::from_mode(0o700));
    }
    let listener = ListenerOptions::new().name(name()?).try_overwrite(true).create_tokio()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(RUNTIME_DIR.join("fane.sock"), std::fs::Permissions::from_mode(0o600));
    }
    loop {
        let conn: TokioStream = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("IPC accept failed: {e}");
                continue;
            }
        };
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(&conn);
            let mut line = String::new();
            if reader.read_line(&mut line).await.is_err() {
                return;
            }
            let req: Value = serde_json::from_str(&line).unwrap_or_else(|_| json!({}));
            let reply = handle(req).await;
            let mut send = &conn;
            let _ = send.write_all(format!("{reply}\n").as_bytes()).await;
            let _ = send.flush().await;
        });
    }
}
