//! Downloads with progress that pick up where they left off (models are many GB).

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;

pub type Progress<'a> = &'a (dyn Fn(&str) + Send + Sync);

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(120))
        .user_agent(concat!("fane/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| crate::llm::http_client(Duration::from_secs(3600)))
}

/// "hf:<user>/<repo>/<path>" → its download URL.
pub fn hf_url(spec: &str) -> Option<String> {
    let rest = spec.strip_prefix("hf:")?;
    let mut parts = rest.splitn(3, '/');
    let (user, repo, path) = (parts.next()?, parts.next()?, parts.next()?);
    Some(format!("https://huggingface.co/{user}/{repo}/resolve/main/{path}"))
}

pub fn gb(bytes: u64) -> String {
    if bytes < 1_000_000_000 {
        format!("{:.0} MB", bytes as f64 / 1e6)
    } else {
        format!("{:.1} GB", bytes as f64 / 1e9)
    }
}

/// Download `url` to `dest` (via `dest.part`, resuming it if a previous attempt stopped).
pub async fn fetch(url: &str, dest: &Path, what: &str, progress: Progress<'_>) -> Result<(), String> {
    if dest.is_file() {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let part = dest.with_extension(format!(
        "{}part",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut attempts = 0;
    loop {
        attempts += 1;
        match fetch_once(url, &part, what, progress).await {
            Ok(()) => break,
            Err((e, retry)) if retry && attempts < 5 => {
                tracing::warn!("download of {what} interrupted ({e}); resuming");
                progress(&format!("Download of {what} interrupted; resuming…"));
                tokio::time::sleep(Duration::from_secs(2 * attempts)).await;
            }
            Err((e, _)) => return Err(e),
        }
    }
    tokio::fs::rename(&part, dest).await.map_err(|e| format!("can't save {}: {e}", dest.display()))
}

/// One attempt; the error says whether retrying could help.
async fn fetch_once(url: &str, part: &Path, what: &str, progress: Progress<'_>) -> Result<(), (String, bool)> {
    let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    let mut req = client().get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req.send().await.map_err(|e| (format!("couldn't download {what}: {e}"), true))?;
    let status = resp.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(()); // already complete
    }
    if !status.is_success() {
        let retry = status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        return Err((format!("couldn't download {what}: HTTP {status} from {url}"), retry));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let start = if resumed { have } else { 0 };
    let total = resp.content_length().map(|n| n + start);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)
        .await
        .map_err(|e| (format!("can't write {}: {e}", part.display()), false))?;
    let (mut done, mut last, began) = (start, Instant::now() - Duration::from_secs(1), Instant::now());
    while let Some(chunk) = resp.chunk().await.map_err(|e| (format!("download of {what} failed: {e}"), true))? {
        file.write_all(&chunk).await.map_err(|e| (format!("can't write {}: {e}", part.display()), false))?;
        done += chunk.len() as u64;
        if last.elapsed() > Duration::from_millis(500) {
            last = Instant::now();
            let rate = (done - start) as f64 / began.elapsed().as_secs_f64().max(0.1);
            let amount = match total {
                Some(t) if t > 0 => format!("{} of {} ({}%)", gb(done), gb(t), done * 100 / t),
                _ => gb(done),
            };
            progress(&format!("Downloading {what}… {amount}, {}/s", gb(rate as u64)));
        }
    }
    file.flush().await.map_err(|e| (e.to_string(), false))?;
    if let Some(t) = total
        && done < t
    {
        return Err((format!("download of {what} stopped at {} of {}", gb(done), gb(t)), true));
    }
    Ok(())
}

/// Unpack a .zip or .tar.gz into `dir`.
pub fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = archive.to_string_lossy().to_lowercase();
    let file = std::fs::File::open(archive).map_err(|e| format!("can't open {}: {e}", archive.display()))?;
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad archive {}: {e}", archive.display()))?;
        zip.extract(dir).map_err(|e| format!("can't unpack {}: {e}", archive.display()))
    } else {
        let gz = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(gz);
        tar.set_preserve_permissions(true);
        tar.unpack(dir).map_err(|e| format!("can't unpack {}: {e}", archive.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hf_urls() {
        assert_eq!(
            hf_url("hf:unsloth/Qwen3.8-27B-GGUF/MTP/mtp-Qwen3.8-27B-Q4_0.gguf").as_deref(),
            Some("https://huggingface.co/unsloth/Qwen3.8-27B-GGUF/resolve/main/MTP/mtp-Qwen3.8-27B-Q4_0.gguf")
        );
        assert_eq!(hf_url("/local/file.gguf"), None);
        assert_eq!(gb(14_618_000_000), "14.6 GB");
        assert_eq!(gb(461_000_000), "461 MB");
    }

    /// A server that hands out a 1000-byte file, honouring Range.
    async fn server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/f.bin", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let from = req
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim_end_matches('-').trim().trim_end_matches('-').parse::<usize>().ok());
                let (head, body) = match from {
                    Some(f) => ("206 Partial Content", &data[f..]),
                    None => ("200 OK", &data[..]),
                };
                let h = format!("HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(h.as_bytes()).await;
                let _ = s.write_all(body).await;
            }
        });
        url
    }

    #[tokio::test]
    async fn downloads_and_resumes() {
        let url = server().await;
        let dir = std::env::temp_dir().join(format!("fane-dl-{}", std::process::id()));
        let dest = dir.join("f.bin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A previous attempt got the first 400 bytes.
        let expected: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("f.bin.part"), &expected[..400]).unwrap();
        fetch(&url, &dest, "f", &|_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), expected);
        assert!(!dir.join("f.bin.part").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
