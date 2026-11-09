//! The GPUs on this computer, and how to point llama.cpp / stable-diffusion.cpp at one.
//!
//! A GPU's id says which backend drives it: "cuda:N" (NVIDIA, CUDA build), "vulkan:N" (any
//! other GPU, Vulkan build), "metal" (Apple) or "cpu". An NVIDIA card also shows up as a Vulkan
//! device; it's listed once, as CUDA, and remembers its Vulkan index for builds without CUDA.

use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Cuda,
    Vulkan,
    Metal,
    Cpu,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cuda => "cuda",
            Backend::Vulkan => "vulkan",
            Backend::Metal => "metal",
            Backend::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gpu {
    pub id: String,
    pub name: String,
    /// Memory it can use, in MiB (shared system memory for integrated GPUs).
    pub vram_mb: u64,
    pub integrated: bool,
    pub cuda: Option<usize>,
    pub vulkan: Option<usize>,
}

impl Gpu {
    pub fn backend(&self) -> Backend {
        match self.id.split(':').next().unwrap_or("") {
            "cuda" => Backend::Cuda,
            "vulkan" => Backend::Vulkan,
            "metal" => Backend::Metal,
            _ => Backend::Cpu,
        }
    }

    pub fn cpu() -> Gpu {
        Gpu {
            id: "cpu".into(),
            name: "CPU (no GPU)".into(),
            vram_mb: system_memory_mb() / 2,
            integrated: true,
            cuda: None,
            vulkan: None,
        }
    }

    /// For pickers: "Tesla V100-SXM2-32GB (32 GB)".
    pub fn label(&self) -> String {
        if self.id == "cpu" {
            return self.name.clone();
        }
        let kind = if self.integrated { ", shared memory" } else { "" };
        format!("{} ({:.0} GB{kind})", self.name, self.vram_mb as f64 / 1024.0)
    }

    /// llama.cpp `--device` for a build of `backend` (None: let it choose, e.g. Metal).
    pub fn llama_device(&self, backend: Backend) -> Option<String> {
        match backend {
            Backend::Cuda => self.cuda.map(|i| format!("CUDA{i}")),
            Backend::Vulkan => match self.vulkan {
                Some(i) => Some(format!("Vulkan{i}")),
                None if self.id == "cpu" => Some("none".into()),
                None => None,
            },
            Backend::Metal => None,
            Backend::Cpu => Some("none".into()),
        }
    }

    /// stable-diffusion.cpp `--backend` for a build of `backend`.
    pub fn sd_device(&self, backend: Backend) -> Option<String> {
        match backend {
            Backend::Cuda => self.cuda.map(|i| format!("cuda{i}")),
            Backend::Vulkan => self.vulkan.map(|i| format!("vulkan{i}")),
            Backend::Metal => None,
            Backend::Cpu => Some("cpu".into()),
        }
    }
}

/// Every GPU on this computer, most capable first, then the CPU.
pub fn detect() -> Vec<Gpu> {
    let cuda = nvidia_smi();
    let vulkan = vulkan_devices();
    let mut gpus = merge(cuda, vulkan);
    if cfg!(target_os = "macos") {
        gpus.push(Gpu {
            id: "metal".into(),
            name: mac_chip().unwrap_or_else(|| "Apple GPU".into()),
            // Metal can use about three quarters of the unified memory.
            vram_mb: system_memory_mb() * 3 / 4,
            integrated: true,
            cuda: None,
            vulkan: None,
        });
    }
    sort(&mut gpus);
    gpus.push(Gpu::cpu());
    gpus
}

/// Most capable first: dedicated before integrated, then by memory.
fn sort(gpus: &mut [Gpu]) {
    gpus.sort_by_key(|g| (g.integrated, std::cmp::Reverse(g.vram_mb)));
}

/// The GPU for a setting: "auto" (the most capable), an id, or a name. Falls back to the most
/// capable GPU when the configured one is gone.
pub fn pick<'a>(gpus: &'a [Gpu], wanted: &str) -> &'a Gpu {
    let wanted = wanted.trim();
    if !wanted.is_empty() && wanted != "auto" {
        if let Some(g) = gpus.iter().find(|g| g.id == wanted || g.name.eq_ignore_ascii_case(wanted)) {
            return g;
        }
        // A Vulkan id that is really an NVIDIA card listed under CUDA.
        if let Some(i) = wanted.strip_prefix("vulkan:").and_then(|i| i.parse::<usize>().ok())
            && let Some(g) = gpus.iter().find(|g| g.vulkan == Some(i))
        {
            return g;
        }
        tracing::warn!("GPU {wanted:?} not found; using the most capable one");
    }
    &gpus[0]
}

fn merge(cuda: Vec<(usize, String, u64)>, vulkan: Vec<VkDevice>) -> Vec<Gpu> {
    let mut gpus: Vec<Gpu> = cuda
        .into_iter()
        .map(|(i, name, mb)| Gpu {
            id: format!("cuda:{i}"),
            name,
            vram_mb: mb,
            integrated: false,
            cuda: Some(i),
            vulkan: None,
        })
        .collect();
    for d in vulkan {
        // The same NVIDIA card through Vulkan: remember the index, don't list it twice.
        if let Some(g) = gpus.iter_mut().find(|g| g.cuda.is_some() && g.vulkan.is_none() && same_name(&g.name, &d.name))
        {
            g.vulkan = Some(d.index);
            continue;
        }
        // Integrated GPUs share system memory; drivers can claim more than is really there.
        let ram_share = system_memory_mb() * 3 / 4;
        let vram_mb = match (d.vram_mb, d.integrated) {
            (Some(mb), true) => mb.min(ram_share),
            (Some(mb), false) => mb,
            (None, _) => system_memory_mb() / 2,
        };
        gpus.push(Gpu {
            id: format!("vulkan:{}", d.index),
            name: d.name,
            vram_mb,
            integrated: d.integrated,
            cuda: None,
            vulkan: Some(d.index),
        });
    }
    gpus
}

fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.to_lowercase().replace("nvidia", "").trim().to_string();
    norm(a) == norm(b)
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// NVIDIA GPUs: (index, name, MiB).
fn nvidia_smi() -> Vec<(usize, String, u64)> {
    let Some(text) = output("nvidia-smi", &["--query-gpu=index,name,memory.total", "--format=csv,noheader,nounits"])
    else {
        return vec![];
    };
    parse_nvidia_smi(&text)
}

fn parse_nvidia_smi(text: &str) -> Vec<(usize, String, u64)> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split(',').map(str::trim);
            let i = parts.next()?.parse().ok()?;
            let name = parts.next()?.to_string();
            let mb = parts.next()?.parse().ok()?;
            Some((i, name, mb))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct VkDevice {
    pub index: usize,
    pub name: String,
    pub integrated: bool,
    pub vram_mb: Option<u64>,
}

/// Vulkan GPUs, numbered the way ggml numbers them (software renderers left out).
fn vulkan_devices() -> Vec<VkDevice> {
    // An installed llama.cpp Vulkan build knows best (its numbering, and memory sizes).
    if let Some(server) =
        super::install::installed_llama(Backend::Vulkan).map(|d| d.join(super::install::exe("llama-server")))
        && let Some(text) = super::install::list_devices(&server)
    {
        let devices = parse_list_devices(&text, "Vulkan");
        if !devices.is_empty() {
            return devices;
        }
    }
    output("vulkaninfo", &["--summary"]).map(|t| parse_vulkaninfo(&t)).unwrap_or_default()
}

/// `vulkaninfo --summary`: "GPU0:" blocks with deviceType and deviceName.
fn parse_vulkaninfo(text: &str) -> Vec<VkDevice> {
    let mut found: Vec<(String, String)> = vec![]; // (type, name)
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("GPU") && t.ends_with(':') && t[3..t.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            found.extend(current.take());
            current = Some((String::new(), String::new()));
        } else if let Some(cur) = current.as_mut()
            && let Some((k, v)) = t.split_once('=')
        {
            match k.trim() {
                "deviceType" => cur.0 = v.trim().to_string(),
                "deviceName" => cur.1 = v.trim().to_string(),
                _ => {}
            }
        }
    }
    found.extend(current);
    found
        .into_iter()
        .filter(|(kind, _)| !kind.contains("CPU"))
        .enumerate()
        .map(|(index, (kind, name))| VkDevice { index, name, integrated: kind.contains("INTEGRATED"), vram_mb: None })
        .collect()
}

/// llama-server / sd-server `--list-devices`, e.g. "  Vulkan0: Intel(R) Iris(R) Xe Graphics (47889 MiB, 43100 MiB free)".
pub fn parse_list_devices(text: &str, prefix: &str) -> Vec<VkDevice> {
    let re = regex::Regex::new(&format!(r"^\s*{prefix}(\d+):\s*(.+?)\s*\((\d+) MiB")).unwrap();
    text.lines()
        .filter_map(|l| {
            let c = re.captures(l)?;
            let name = c[2].to_string();
            let lower = name.to_lowercase();
            let integrated = lower.contains("intel") && !lower.contains("arc")
                || lower.contains("radeon(tm) graphics")
                || lower.contains("llvmpipe");
            Some(VkDevice { index: c[1].parse().ok()?, name, integrated, vram_mb: c[3].parse().ok() })
        })
        .filter(|d| !d.name.to_lowercase().contains("llvmpipe"))
        .collect()
}

#[cfg(target_os = "macos")]
fn mac_chip() -> Option<String> {
    output("sysctl", &["-n", "machdep.cpu.brand_string"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn mac_chip() -> Option<String> {
    None
}

/// Total RAM in MiB (8 GiB if unknown).
pub fn system_memory_mb() -> u64 {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo")
        && let Some(kb) = text
            .lines()
            .find(|l| l.starts_with("MemTotal:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<u64>().ok())
    {
        return kb / 1024;
    }
    #[cfg(target_os = "macos")]
    if let Some(b) = output("sysctl", &["-n", "hw.memsize"]).and_then(|s| s.trim().parse::<u64>().ok()) {
        return b / 1024 / 1024;
    }
    #[cfg(target_os = "windows")]
    if let Some(b) =
        output("powershell", &["-NoProfile", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
            .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return b / 1024 / 1024;
    }
    8192
}

#[cfg(test)]
mod tests {
    use super::*;

    const VULKANINFO: &str = "Devices:\n========\nGPU0:\n\tapiVersion         = 1.3.274\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU\n\tdeviceName         = Intel(R) Iris(R) Xe Graphics (RPL-P)\nGPU1:\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n\tdeviceName         = Tesla V100-SXM2-32GB\nGPU2:\n\tdeviceType         = PHYSICAL_DEVICE_TYPE_CPU\n\tdeviceName         = llvmpipe (LLVM 20.1.2, 256 bits)\n";

    #[test]
    fn parses_tools_output() {
        assert_eq!(
            parse_nvidia_smi("0, Tesla V100-SXM2-32GB, 32768\n"),
            vec![(0, "Tesla V100-SXM2-32GB".to_string(), 32768)]
        );
        let vk = parse_vulkaninfo(VULKANINFO);
        assert_eq!(vk.len(), 2);
        assert!(vk[0].integrated && vk[0].name.starts_with("Intel"));
        assert_eq!((vk[1].index, vk[1].integrated), (1, false));
        let listed = parse_list_devices(
            "Available devices:\n  Vulkan0: Intel(R) Iris(R) Xe Graphics (RPL-P) (47889 MiB, 43100 MiB free)\n  Vulkan1: Tesla V100-SXM2-32GB (32768 MiB, 29012 MiB free)\n",
            "Vulkan",
        );
        assert_eq!(listed[0].vram_mb, Some(47889));
        assert!(listed[0].integrated && !listed[1].integrated);
    }

    #[test]
    fn nvidia_card_is_listed_once_and_first() {
        let mut gpus = merge(vec![(0, "Tesla V100-SXM2-32GB".into(), 32768)], parse_vulkaninfo(VULKANINFO));
        sort(&mut gpus);
        gpus.push(Gpu::cpu());
        let ids: Vec<&str> = gpus.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["cuda:0", "vulkan:0", "cpu"]);
        assert_eq!(gpus[0].vulkan, Some(1));
        assert_eq!(gpus[0].llama_device(Backend::Cuda).as_deref(), Some("CUDA0"));
        assert_eq!(gpus[0].llama_device(Backend::Vulkan).as_deref(), Some("Vulkan1"));
        assert_eq!(gpus[0].sd_device(Backend::Vulkan).as_deref(), Some("vulkan1"));
        assert_eq!(gpus[1].llama_device(Backend::Vulkan).as_deref(), Some("Vulkan0"));
        assert_eq!(pick(&gpus, "auto").id, "cuda:0");
        assert_eq!(pick(&gpus, "vulkan:0").id, "vulkan:0");
        assert_eq!(pick(&gpus, "vulkan:1").id, "cuda:0", "the V100's Vulkan id");
        assert_eq!(pick(&gpus, "gone:3").id, "cuda:0");
        assert_eq!(gpus[0].label(), "Tesla V100-SXM2-32GB (32 GB)");
    }
}
