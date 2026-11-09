//! llama.cpp and stable-diffusion.cpp builds: which release asset fits this computer, and
//! downloading and unpacking it under `~/.fane/runtimes`. stable-diffusion.cpp's own Linux
//! builds need a newer glibc than most distributions have, so on Linux it comes from Fane's
//! builds, or is compiled here when the tools for that are installed.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use super::download::{self, Progress};
use super::gpu::Backend;

/// The releases this version of Fane was tested with.
pub const LLAMA_BUILD: &str = "b12233";
pub const SD_TAG: &str = "master-1079-a3c49b1";

const LLAMA_REPO: &str = "ggml-org/llama.cpp";
const SD_REPO: &str = "leejet/stable-diffusion.cpp";
/// Fane's own stable-diffusion.cpp builds for Linux (glibc 2.35+, CUDA 12 and Vulkan).
const FANE_REPO: &str = "salsicha/Fane";

static HOME: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Set where runtimes and models live (from `[runtime] home`).
pub fn set_home(dir: PathBuf) {
    *HOME.write().unwrap() = Some(dir);
}

pub fn home() -> PathBuf {
    HOME.read().unwrap().clone().unwrap_or_else(|| crate::config::expand("~/.fane"))
}

pub fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn llama_dir(build: &str, backend: Backend) -> PathBuf {
    home().join("runtimes/llama.cpp").join(format!("{build}-{}", backend.as_str()))
}

fn sd_dir(tag: &str, backend: Backend) -> PathBuf {
    home().join("runtimes/sd.cpp").join(format!("{tag}-{}", backend.as_str()))
}

/// The llama.cpp build to use for a backend: the CPU runs on the Vulkan build (it has the CPU
/// backend too), and Linux/Windows have no Metal.
pub fn llama_backend_for(gpu: Backend) -> Backend {
    match gpu {
        Backend::Cpu if cfg!(target_os = "macos") => Backend::Metal,
        Backend::Cpu => Backend::Vulkan,
        b => b,
    }
}

/// The directory holding an installed llama-server for `backend`: the pinned build, or else
/// the newest one installed.
pub fn installed_llama(backend: Backend) -> Option<PathBuf> {
    installed(&home().join("runtimes/llama.cpp"), LLAMA_BUILD, backend, "llama-server")
}

pub fn installed_sd(backend: Backend) -> Option<PathBuf> {
    installed(&home().join("runtimes/sd.cpp"), SD_TAG, backend, "sd-server")
}

fn installed(root: &Path, pinned: &str, backend: Backend, binary: &str) -> Option<PathBuf> {
    let pinned_dir = root.join(format!("{pinned}-{}", backend.as_str()));
    if pinned_dir.join(exe(binary)).is_file() {
        return Some(pinned_dir);
    }
    let suffix = format!("-{}", backend.as_str());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix)))
        .filter(|p| p.join(exe(binary)).is_file())
        .collect();
    dirs.sort();
    dirs.pop()
}

/// A command for a runtime binary, with its bundled libraries findable.
pub fn command(bin: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
    if let Some(dir) = bin.parent() {
        lib_path(dir, |k, v| {
            cmd.env(k, v);
        });
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // no console window
    }
    // If Fane dies without stopping it, the model server goes too (and frees the GPU).
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd
}

fn lib_path(dir: &Path, mut set: impl FnMut(&str, String)) {
    let var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let sep = if cfg!(windows) { ";" } else { ":" };
    let old = std::env::var(var).unwrap_or_default();
    set(var, if old.is_empty() { dir.display().to_string() } else { format!("{}{sep}{old}", dir.display()) });
}

/// `<server> --list-devices` output (llama-server and sd-server both have it).
pub fn list_devices(server: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(server);
    cmd.arg("--list-devices");
    if let Some(dir) = server.parent() {
        lib_path(dir, |k, v| {
            cmd.env(k, v);
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    out.status.success().then_some(text)
}

/// Whether a binary starts at all here (catches builds for a newer C library).
fn runs(bin: &Path) -> bool {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("--help");
    if let Some(dir) = bin.parent() {
        lib_path(dir, |k, v| {
            cmd.env(k, v);
        });
    }
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// Release assets (regexes over asset names) for llama.cpp on this OS and CPU.
fn llama_assets(backend: Backend) -> Result<Vec<&'static str>, String> {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    Ok(match (os, arch, backend) {
        ("linux", "x86_64", Backend::Cuda) => vec![
            r"^llama-b\d+-bin-ubuntu-cuda-12\.[\d.]+-x64\.tar\.gz$",
            r"^cudart-llama-b\d+-bin-ubuntu-cuda-12\.[\d.]+-x64\.tar\.gz$",
        ],
        ("linux", "x86_64", _) => vec![r"^llama-b\d+-bin-ubuntu-vulkan-x64\.tar\.gz$"],
        ("linux", "aarch64", _) => vec![r"^llama-b\d+-bin-ubuntu-vulkan-arm64\.tar\.gz$"],
        ("macos", "aarch64", _) => vec![r"^llama-b\d+-bin-macos-arm64\.tar\.gz$"],
        ("macos", "x86_64", _) => vec![r"^llama-b\d+-bin-macos-x64\.tar\.gz$"],
        ("windows", "x86_64", Backend::Cuda) => {
            vec![r"^llama-b\d+-bin-win-cuda-12\.[\d.]+-x64\.zip$", r"^cudart-llama-bin-win-cuda-12\.[\d.]+-x64\.zip$"]
        }
        ("windows", "x86_64", _) => vec![r"^llama-b\d+-bin-win-vulkan-x64\.zip$"],
        ("windows", "aarch64", _) => vec![r"^llama-b\d+-bin-win-vulkan-arm64\.zip$"],
        _ => return Err(format!("llama.cpp has no ready-made build for {os} on {arch}")),
    })
}

/// stable-diffusion.cpp's own release assets for this OS (none usable on Linux; see `install_sd`).
fn sd_assets(backend: Backend) -> Option<Vec<&'static str>> {
    match (std::env::consts::OS, std::env::consts::ARCH, backend) {
        ("macos", "aarch64", _) => Some(vec![r"^sd-.*-bin-Darwin-macOS-[\d.]+-arm64\.zip$"]),
        ("windows", "x86_64", Backend::Cuda) => {
            Some(vec![r"^sd-.*-bin-win-cuda12-x64\.zip$", r"^cudart-sd-bin-win-cu12-x64\.zip$"])
        }
        ("windows", "x86_64", _) => Some(vec![r"^sd-.*-bin-win-vulkan-x64\.zip$"]),
        ("linux", "x86_64", Backend::Vulkan | Backend::Cpu) => {
            Some(vec![r"^sd-.*-bin-Linux-Ubuntu-[\d.]+-x86_64-vulkan\.zip$"])
        }
        _ => None,
    }
}

/// (name, download URL) of a release's assets.
async fn release_assets(repo: &str, tag: &str) -> Result<Vec<(String, String)>, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
    let client = crate::llm::http_client(std::time::Duration::from_secs(30));
    let resp = client
        .get(&url)
        .header("User-Agent", concat!("fane/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("couldn't reach GitHub: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{repo} release {tag}: HTTP {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some((x["name"].as_str()?.to_string(), x["browser_download_url"].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn pick_assets(assets: &[(String, String)], patterns: &[&str]) -> Result<Vec<(String, String)>, String> {
    patterns
        .iter()
        .map(|p| {
            let re = regex::Regex::new(p).unwrap();
            assets.iter().find(|(n, _)| re.is_match(n)).cloned().ok_or_else(|| format!("no release file matches {p}"))
        })
        .collect()
}

/// Download and unpack `assets` into `dir`, flattening the archives' top folders so the
/// binaries and their libraries sit together.
async fn fetch_into(assets: &[(String, String)], dir: &Path, what: &str, progress: Progress<'_>) -> Result<(), String> {
    let tmp = dir.with_extension("download");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let staging = tmp.join("unpacked");
    for (name, url) in assets {
        let archive = tmp.join(name);
        download::fetch(url, &archive, what, progress).await?;
        progress(&format!("Unpacking {what}…"));
        let (a, s) = (archive.clone(), staging.clone());
        tokio::task::spawn_blocking(move || download::unpack(&a, &s)).await.map_err(|e| e.to_string())??;
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    flatten(&staging, dir)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

/// Move every file under `from` (any depth) directly into `to`.
fn flatten(from: &Path, to: &Path) -> Result<(), String> {
    for entry in walkdir::WalkDir::new(from).into_iter().filter_map(|e| e.ok()) {
        let ft = entry.file_type();
        if ft.is_dir() {
            continue;
        }
        let dest = to.join(entry.file_name());
        if ft.is_symlink() {
            #[cfg(unix)]
            if let Ok(target) = std::fs::read_link(entry.path()) {
                let _ = std::fs::remove_file(&dest);
                let _ = std::os::unix::fs::symlink(target, &dest);
            }
            continue;
        }
        std::fs::rename(entry.path(), &dest)
            .or_else(|_| std::fs::copy(entry.path(), &dest).map(|_| ()))
            .map_err(|e| format!("can't install {}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Install llama.cpp for `backend` (if it isn't already) and return its directory.
pub async fn install_llama(backend: Backend, build: &str, progress: Progress<'_>) -> Result<PathBuf, String> {
    let backend = llama_backend_for(backend);
    let build = if build.is_empty() { LLAMA_BUILD } else { build };
    let dir = llama_dir(build, backend);
    if dir.join(exe("llama-server")).is_file() {
        return Ok(dir);
    }
    let what = format!("llama.cpp ({})", backend.as_str());
    progress(&format!("Getting {what}…"));
    let assets = pick_assets(&release_assets(LLAMA_REPO, build).await?, &llama_assets(backend)?)?;
    fetch_into(&assets, &dir, &what, progress).await?;
    let server = dir.join(exe("llama-server"));
    if !server.is_file() {
        return Err(format!("the {what} download has no llama-server"));
    }
    if backend == Backend::Cuda && !list_devices(&server).is_some_and(|t| t.contains("CUDA0")) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("llama.cpp's CUDA build can't see the NVIDIA GPU (the driver may be too old)".into());
    }
    Ok(dir)
}

/// Install stable-diffusion.cpp for `backend` and return its directory. Tries the project's own
/// build, then Fane's Linux build, then compiling it here.
pub async fn install_sd(backend: Backend, tag: &str, progress: Progress<'_>) -> Result<PathBuf, String> {
    let backend = llama_backend_for(backend);
    let tag = if tag.is_empty() { SD_TAG } else { tag };
    let dir = sd_dir(tag, backend);
    if dir.join(exe("sd-server")).is_file() {
        return Ok(dir);
    }
    let what = format!("stable-diffusion.cpp ({})", backend.as_str());
    progress(&format!("Getting {what}…"));
    let mut errors = vec![];
    if let Some(patterns) = sd_assets(backend) {
        match release_assets(SD_REPO, tag).await.and_then(|a| pick_assets(&a, &patterns)) {
            Ok(assets) => match fetch_into(&assets, &dir, &what, progress).await {
                Ok(()) if runs(&dir.join(exe("sd-server"))) => return Ok(dir),
                Ok(()) => errors.push("the ready-made build doesn't run on this system".to_string()),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(e),
        }
    }
    if cfg!(target_os = "linux") {
        let short = short_tag(tag);
        let variant = if backend == Backend::Cuda { "cuda12" } else { "vulkan" };
        let name = format!("sd-{short}-linux-{variant}-x64.tar.gz");
        let fane_tag = format!("sd-{short}");
        match release_assets(FANE_REPO, &fane_tag).await.and_then(|a| pick_assets(&a, &[&regex::escape(&name)])) {
            Ok(assets) => match fetch_into(&assets, &dir, &what, progress).await {
                Ok(()) if runs(&dir.join(exe("sd-server"))) => return Ok(dir),
                Ok(()) => errors.push("Fane's build doesn't run on this system".to_string()),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(e),
        }
        match build_sd(tag, backend, &dir, progress).await {
            Ok(()) => return Ok(dir),
            Err(e) => errors.push(e),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Err(format!("couldn't install {what}: {}", errors.join("; ")))
}

/// "master-1079-a3c49b1" → "master-a3c49b1" (the name used in asset file names).
fn short_tag(tag: &str) -> String {
    let parts: Vec<&str> = tag.split('-').collect();
    if parts.len() == 3 { format!("{}-{}", parts[0], parts[2]) } else { tag.to_string() }
}

/// What compiling stable-diffusion.cpp here needs that's missing (empty: ready).
pub fn build_requirements(backend: Backend) -> Vec<&'static str> {
    let mut missing = vec![];
    for (tool, pkg) in [("git", "git"), ("cmake", "cmake")] {
        if which::which(tool).is_err() {
            missing.push(pkg);
        }
    }
    if which::which("c++").is_err() && which::which("g++").is_err() && which::which("clang++").is_err() {
        missing.push("g++");
    }
    match backend {
        Backend::Cuda => {
            if nvcc().is_none() {
                missing.push("the CUDA toolkit (nvcc)");
            }
        }
        _ => {
            if which::which("glslc").is_err() {
                missing.push("glslc");
            }
            if !Path::new("/usr/include/vulkan/vulkan.h").is_file() {
                missing.push("libvulkan-dev");
            }
        }
    }
    missing
}

fn nvcc() -> Option<PathBuf> {
    which::which("nvcc").ok().or_else(|| Some(PathBuf::from("/usr/local/cuda/bin/nvcc")).filter(|p| p.is_file()))
}

async fn build_sd(tag: &str, backend: Backend, dir: &Path, progress: Progress<'_>) -> Result<(), String> {
    let missing = build_requirements(backend);
    if !missing.is_empty() {
        return Err(format!("compiling it here needs {}", missing.join(", ")));
    }
    let src = home().join("runtimes/build").join(format!("sd.cpp-{tag}"));
    let run = |mut cmd: tokio::process::Command, what: &'static str| async move {
        let out = cmd.output().await.map_err(|e| format!("{what}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(5).collect();
            Err(format!("{what} failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" / ")))
        }
    };
    if !src.join("CMakeLists.txt").is_file() {
        progress("Downloading stable-diffusion.cpp's source…");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut git = tokio::process::Command::new("git");
        git.args(["clone", "--depth", "1", "--branch", tag, "--recurse-submodules", "--shallow-submodules"])
            .arg(format!("https://github.com/{SD_REPO}"))
            .arg(&src);
        run(git, "downloading the source").await?;
    }
    progress("Compiling stable-diffusion.cpp (takes a few minutes)…");
    let build = src.join(format!("build-{}", backend.as_str()));
    let mut cmake = tokio::process::Command::new("cmake");
    cmake.arg("-S").arg(&src).arg("-B").arg(&build).args(["-DCMAKE_BUILD_TYPE=Release", "-DSD_BUILD_SHARED_LIBS=OFF"]);
    match backend {
        Backend::Cuda => {
            cmake.args(["-DSD_CUDA=ON", "-DCMAKE_CUDA_ARCHITECTURES=native"]);
            if let Some(nvcc) = nvcc() {
                cmake.arg(format!("-DCMAKE_CUDA_COMPILER={}", nvcc.display()));
            }
        }
        _ => {
            cmake.arg("-DSD_VULKAN=ON");
        }
    }
    run(cmake, "configuring the build").await?;
    let jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).to_string();
    let mut make = tokio::process::Command::new("cmake");
    make.arg("--build").arg(&build).args(["--config", "Release", "-j", &jobs, "--target", "sd-server", "sd-cli"]);
    run(make, "compiling").await?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(build.join("bin")).map_err(|e| e.to_string())?.flatten() {
        std::fs::copy(entry.path(), dir.join(entry.file_name())).map_err(|e| e.to_string())?;
    }
    // Shared ggml libraries, if the build made any.
    for entry in walkdir::WalkDir::new(&build).max_depth(4).into_iter().filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file() && (name.ends_with(".so") || name.contains(".so.")) {
            let _ = std::fs::copy(entry.path(), dir.join(entry.file_name()));
        }
    }
    if runs(&dir.join(exe("sd-server"))) { Ok(()) } else { Err("the compiled sd-server doesn't start".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_patterns_match_real_release_names() {
        let names = [
            "llama-b12233-bin-ubuntu-cuda-12.8-x64.tar.gz",
            "cudart-llama-b12233-bin-ubuntu-cuda-12.8-x64.tar.gz",
            "llama-b12233-bin-ubuntu-cuda-13.5-x64.tar.gz",
            "llama-b12233-bin-ubuntu-vulkan-x64.tar.gz",
            "llama-b12233-bin-macos-arm64.tar.gz",
            "llama-b12233-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "llama-b12233-bin-win-vulkan-x64.zip",
            "sd-master-a3c49b1-bin-Linux-Ubuntu-26.04-x86_64-vulkan.zip",
            "sd-master-a3c49b1-bin-win-cuda12-x64.zip",
            "cudart-sd-bin-win-cu12-x64.zip",
            "sd-master-a3c49b1-bin-Darwin-macOS-26.7.2-arm64.zip",
        ];
        let assets: Vec<(String, String)> = names.iter().map(|n| (n.to_string(), format!("https://x/{n}"))).collect();
        let all = [
            r"^llama-b\d+-bin-ubuntu-cuda-12\.[\d.]+-x64\.tar\.gz$",
            r"^cudart-llama-b\d+-bin-ubuntu-cuda-12\.[\d.]+-x64\.tar\.gz$",
            r"^llama-b\d+-bin-ubuntu-vulkan-x64\.tar\.gz$",
            r"^llama-b\d+-bin-macos-arm64\.tar\.gz$",
            r"^llama-b\d+-bin-win-cuda-12\.[\d.]+-x64\.zip$",
            r"^cudart-llama-bin-win-cuda-12\.[\d.]+-x64\.zip$",
            r"^llama-b\d+-bin-win-vulkan-x64\.zip$",
            r"^sd-.*-bin-Linux-Ubuntu-[\d.]+-x86_64-vulkan\.zip$",
            r"^sd-.*-bin-win-cuda12-x64\.zip$",
            r"^cudart-sd-bin-win-cu12-x64\.zip$",
            r"^sd-.*-bin-Darwin-macOS-[\d.]+-arm64\.zip$",
        ];
        let picked = pick_assets(&assets, &all).unwrap();
        assert_eq!(picked[0].0, "llama-b12233-bin-ubuntu-cuda-12.8-x64.tar.gz", "CUDA 12, not 13 (Volta)");
        assert!(llama_assets(Backend::Vulkan).is_ok());
        assert!(pick_assets(&assets, &[r"^nothing$"]).is_err());
        assert_eq!(short_tag("master-1079-a3c49b1"), "master-a3c49b1");
    }

    #[test]
    fn finds_installed_builds() {
        let root = std::env::temp_dir().join(format!("fane-inst-{}", std::process::id()));
        let old = root.join(format!("b100-{}", Backend::Vulkan.as_str()));
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(installed(&root, "b200", Backend::Vulkan, "llama-server"), None);
        std::fs::write(old.join(exe("llama-server")), b"").unwrap();
        assert_eq!(installed(&root, "b200", Backend::Vulkan, "llama-server"), Some(old.clone()));
        assert_eq!(installed(&root, "b200", Backend::Cuda, "llama-server"), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
