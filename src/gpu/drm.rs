//! DRM fdinfo based GPU monitoring.
//!
//! Every open DRM file descriptor exposes per-client counters in
//! `/proc/<pid>/fdinfo/<fd>` (see the kernel's `drm-usage-stats.rst`):
//!
//! ```text
//! drm-pdev:               0000:00:02.0
//! drm-client-id:          22
//! drm-engine-render:      32080060914 ns      (busy time, i915/amdgpu/msm/...)
//! drm-cycles-rcs:         12345               (busy cycles, xe)
//! drm-total-cycles-rcs:   67890
//! drm-engine-capacity-vcs: 2
//! drm-resident-system0:   282980 KiB
//! ```
//!
//! Engine utilisation is the sum over clients of busy-time deltas divided by
//! wall-clock time (or busy-cycles over total-cycles for xe).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{Engine, EngineKind, GpuProcess, GpuSample};

/// (pdev, client id) uniquely identifies a DRM client. Several fds (or forked
/// processes) can share a client; they report identical counters.
type ClientKey = (String, u64);

#[derive(Clone, Copy, Default)]
struct EngineCounter {
    busy: u64,
    total_cycles: Option<u64>,
}

#[derive(Default)]
struct ClientStats {
    /// First process seen holding this client (forked children share it).
    pid: u32,
    engines: HashMap<String, EngineCounter>,
    capacity: HashMap<String, u32>,
    /// memory region -> resident bytes
    memory: HashMap<String, u64>,
    /// Bytes allocated across all regions (drm-total-*), resident or not.
    allocated: u64,
}

struct Card {
    pdev: String,
    driver: String,
    name: String,
    device: PathBuf,
    sysfs: PathBuf,
    hwmon: Option<PathBuf>,
    /// Engines in display order; seeded per driver and extended as seen.
    engines: Vec<String>,
}

pub struct DrmMonitor {
    cards: Vec<Card>,
    prev: HashMap<ClientKey, ClientStats>,
    prev_time: Option<Instant>,
    /// Non-root users can only read fdinfo of their own processes.
    is_root: bool,
}

impl DrmMonitor {
    pub fn new(skip_nvidia: bool) -> Self {
        let mut cards = discover_cards();
        if skip_nvidia {
            cards.retain(|c| c.driver != "nvidia");
        }
        Self {
            cards,
            prev: HashMap::new(),
            prev_time: None,
            is_root: fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0),
        }
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        if self.cards.is_empty() {
            return Vec::new();
        }
        let now = Instant::now();
        let pdevs: HashSet<&str> = self.cards.iter().map(|c| c.pdev.as_str()).collect();
        let cur = scan_clients(&pdevs);
        let dt = self.prev_time.map(|t| now.duration_since(t).as_secs_f64());

        let mut out = Vec::with_capacity(self.cards.len());
        for card in &mut self.cards {
            // engine id -> accumulated busy ratio (1.0 == one engine instance fully busy)
            let mut busy: HashMap<String, f64> = HashMap::new();
            let mut vram = 0u64;
            let mut shared = 0u64;
            let mut allocated = 0u64;
            let mut clients = 0usize;
            // pid -> (per-engine busy ratio, process totals)
            let mut procs: HashMap<u32, (HashMap<&str, f64>, GpuProcess)> = HashMap::new();

            for (key, stats) in cur.iter().filter(|(k, _)| k.0 == card.pdev) {
                clients += 1;
                allocated += stats.allocated;
                let (proc_busy, proc) = procs.entry(stats.pid).or_insert_with(|| {
                    let p = GpuProcess {
                        pid: stats.pid,
                        ..Default::default()
                    };
                    (HashMap::new(), p)
                });
                for (region, bytes) in &stats.memory {
                    if is_vram_region(region) {
                        vram += bytes;
                        proc.vram += bytes;
                    } else {
                        shared += bytes;
                        proc.shared += bytes;
                    }
                }
                for engine in stats.engines.keys() {
                    if !card.engines.contains(engine) {
                        card.engines.push(engine.clone());
                    }
                }
                let (Some(prev), Some(dt)) = (self.prev.get(key), dt) else {
                    continue;
                };
                for (engine, c) in &stats.engines {
                    let Some(p) = prev.engines.get(engine) else {
                        continue;
                    };
                    let delta = c.busy.saturating_sub(p.busy) as f64;
                    let ratio = match (c.total_cycles, p.total_cycles) {
                        (Some(t), Some(pt)) if t > pt => delta / (t - pt) as f64,
                        (Some(_), Some(_)) => 0.0,
                        _ => delta / (dt * 1e9),
                    };
                    let cap = stats.capacity.get(engine).copied().unwrap_or(1).max(1) as f64;
                    *busy.entry(engine.clone()).or_default() += ratio / cap;
                    *proc_busy.entry(engine.as_str()).or_default() += ratio / cap;
                }
            }

            let processes = procs
                .into_values()
                .map(|(engines, mut p)| {
                    for (engine, ratio) in engines {
                        let pct = (ratio * 100.0).clamp(0.0, 100.0);
                        let slot = if engine_kind(engine).is_media() {
                            &mut p.media
                        } else {
                            &mut p.gpu
                        };
                        *slot = slot.max(pct);
                    }
                    p
                })
                .collect();

            card.engines.sort_by_key(|e| (engine_kind(e), e.clone()));
            let mut engines: Vec<Engine> = card
                .engines
                .iter()
                .map(|e| Engine {
                    id: e.clone(),
                    label: engine_label(e),
                    kind: engine_kind(e),
                    busy: (busy.get(e).copied().unwrap_or(0.0) * 100.0).clamp(0.0, 100.0),
                })
                .collect();

            // amdgpu exposes a device-wide busy counter; use it when fdinfo shows nothing.
            if engines.is_empty()
                && let Some(b) = read_num(&card.device.join("gpu_busy_percent"))
            {
                engines.push(Engine {
                    id: "gpu".into(),
                    label: "GPU".into(),
                    kind: EngineKind::Graphics,
                    busy: b as f64,
                });
            }

            let mut s = GpuSample {
                id: card.pdev.clone(),
                name: card.name.clone(),
                driver: card.driver.clone(),
                engines,
                processes,
                vram_used: (vram > 0).then_some(vram),
                shared_used: Some(shared),
                allocated: (allocated > 0).then_some(allocated),
                note: Some(if self.is_root {
                    format!("{clients} DRM clients")
                } else {
                    format!("{clients} DRM clients (yours only)")
                }),
                ..Default::default()
            };
            sysfs_extras(card, &mut s);
            out.push(s);
        }

        self.prev = cur;
        self.prev_time = Some(now);
        out
    }
}

fn discover_cards() -> Vec<Card> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut cards: Vec<Card> = entries
        .flatten()
        .filter_map(|e| {
            let fname = e.file_name();
            let name = fname.to_str()?;
            let n = name.strip_prefix("card")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None; // connectors like card1-eDP-1
            }
            let sysfs = e.path();
            let device = fs::canonicalize(sysfs.join("device")).ok()?;
            let pdev = device.file_name()?.to_str()?.to_string();
            let driver = fs::read_link(device.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "unknown".into());
            let hwmon = fs::read_dir(device.join("hwmon"))
                .ok()
                .and_then(|mut d| d.next())
                .and_then(|e| e.ok())
                .map(|e| e.path());
            Some(Card {
                name: pci_name(&device).unwrap_or_else(|| format!("{driver} GPU")),
                engines: default_engines(&driver),
                pdev,
                driver,
                device,
                sysfs,
                hwmon,
            })
        })
        .collect();
    cards.sort_by(|a, b| a.pdev.cmp(&b.pdev));
    cards.dedup_by(|a, b| a.pdev == b.pdev);
    cards
}

fn default_engines(driver: &str) -> Vec<String> {
    let names: &[&str] = match driver {
        "i915" => &["render", "copy", "video", "video-enhance"],
        "xe" => &["rcs", "bcs", "vcs", "vecs", "ccs"],
        "amdgpu" => &["gfx", "compute", "dma", "dec", "enc"],
        _ => &[],
    };
    names.iter().map(|s| s.to_string()).collect()
}

/// Reads every DRM fd in /proc. Unreadable processes (other users) are skipped.
fn scan_clients(pdevs: &HashSet<&str>) -> HashMap<ClientKey, ClientStats> {
    let mut out = HashMap::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return out;
    };
    let mut procs: Vec<(u32, PathBuf)> = procs
        .flatten()
        .filter_map(|p| Some((p.file_name().to_str()?.parse().ok()?, p.path())))
        .collect();
    // Parents usually have lower pids, so shared clients get attributed to them.
    procs.sort_unstable_by_key(|(pid, _)| *pid);
    for (pid, path) in procs {
        let Ok(fds) = fs::read_dir(path.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            if !target.starts_with("/dev/dri/") {
                continue;
            }
            let Ok(text) = fs::read_to_string(path.join("fdinfo").join(fd.file_name())) else {
                continue;
            };
            if let Some((key, mut stats)) = parse_fdinfo(&text)
                && pdevs.contains(key.0.as_str())
            {
                stats.pid = pid;
                out.entry(key).or_insert(stats);
            }
        }
    }
    out
}

fn parse_fdinfo(text: &str) -> Option<(ClientKey, ClientStats)> {
    let mut pdev = None;
    let mut id = None;
    let mut s = ClientStats::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if k == "drm-pdev" {
            pdev = Some(v.to_string());
        } else if k == "drm-client-id" {
            id = v.parse().ok();
        } else if let Some(e) = k.strip_prefix("drm-engine-capacity-") {
            s.capacity.insert(e.into(), v.parse().unwrap_or(1));
        } else if let Some(e) = k.strip_prefix("drm-engine-") {
            if let Some(n) = first_num(v) {
                s.engines.entry(e.into()).or_default().busy = n;
            }
        } else if let Some(e) = k.strip_prefix("drm-total-cycles-") {
            if let Some(n) = first_num(v) {
                s.engines.entry(e.into()).or_default().total_cycles = Some(n);
            }
        } else if k.starts_with("drm-total-") {
            if let Some(b) = parse_bytes(v) {
                s.allocated += b;
            }
        } else if let Some(e) = k.strip_prefix("drm-cycles-") {
            if let Some(n) = first_num(v) {
                s.engines.entry(e.into()).or_default().busy = n;
            }
        } else if let Some(r) = k.strip_prefix("drm-resident-") {
            if let Some(b) = parse_bytes(v) {
                s.memory.insert(r.into(), b);
            }
        } else if let Some(r) = k.strip_prefix("drm-memory-") {
            // Legacy amdgpu key, superseded by drm-resident-* when both exist.
            if let Some(b) = parse_bytes(v) {
                s.memory.entry(r.into()).or_insert(b);
            }
        }
    }
    Some(((pdev?, id?), s))
}

fn first_num(v: &str) -> Option<u64> {
    v.split_whitespace().next()?.parse().ok()
}

fn parse_bytes(v: &str) -> Option<u64> {
    let mut it = v.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    let mult = match it.next() {
        Some("KiB") => 1 << 10,
        Some("MiB") => 1 << 20,
        Some("GiB") => 1 << 30,
        _ => 1,
    };
    Some(n * mult)
}

fn is_vram_region(region: &str) -> bool {
    region.contains("vram") || region.contains("local")
}

fn engine_kind(e: &str) -> EngineKind {
    match e {
        "render" | "gfx" | "rcs" | "gpu" => EngineKind::Graphics,
        "compute" | "ccs" => EngineKind::Compute,
        "copy" | "bcs" | "dma" | "sdma" => EngineKind::Copy,
        "video" | "vcs" => EngineKind::Video,
        "video-enhance" | "vecs" | "vpe" => EngineKind::VideoEnhance,
        e if e.starts_with("enc") => EngineKind::Encode,
        e if e.starts_with("dec") || e.starts_with("jpeg") => EngineKind::Decode,
        _ => EngineKind::Other,
    }
}

fn engine_label(e: &str) -> String {
    match e {
        "render" | "rcs" => "Render/3D".into(),
        "gfx" => "Graphics".into(),
        "compute" | "ccs" => "Compute".into(),
        "copy" | "bcs" => "Blitter".into(),
        "dma" => "DMA".into(),
        // Intel has no separate encode/decode engines: VCS does both.
        "video" | "vcs" => "Video enc/dec".into(),
        "video-enhance" | "vecs" => "VideoEnhance".into(),
        "dec" => "Decode".into(),
        "enc" => "Encode".into(),
        "jpeg" => "JPEG".into(),
        other => other.into(),
    }
}

fn read_num(p: &Path) -> Option<u64> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn sysfs_extras(card: &Card, s: &mut GpuSample) {
    match card.driver.as_str() {
        "i915" => {
            s.freq_mhz = read_num(&card.sysfs.join("gt_act_freq_mhz")).map(|v| v as u32);
            s.max_freq_mhz = read_num(&card.sysfs.join("gt_max_freq_mhz")).map(|v| v as u32);
        }
        "xe" => {
            let f = card.device.join("tile0/gt0/freq0");
            s.freq_mhz = read_num(&f.join("act_freq")).map(|v| v as u32);
            s.max_freq_mhz = read_num(&f.join("max_freq")).map(|v| v as u32);
        }
        "amdgpu" => {
            // Device-wide numbers are more accurate than summing visible clients.
            if let Some(v) = read_num(&card.device.join("mem_info_vram_used")) {
                s.vram_used = Some(v);
            }
            s.vram_total = read_num(&card.device.join("mem_info_vram_total"));
            if let Some(v) = read_num(&card.device.join("mem_info_gtt_used")) {
                s.shared_used = Some(v);
            }
            if let Ok(t) = fs::read_to_string(card.device.join("pp_dpm_sclk")) {
                let mhz = |l: &str| {
                    l.split_whitespace()
                        .nth(1)?
                        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                        .parse()
                        .ok()
                };
                s.freq_mhz = t.lines().find(|l| l.ends_with('*')).and_then(mhz);
                s.max_freq_mhz = t.lines().last().and_then(mhz);
            }
        }
        _ => {}
    }
    if let Some(h) = &card.hwmon {
        s.temp_c = read_num(&h.join("temp1_input")).map(|m| m as f32 / 1000.0);
        s.power_w = read_num(&h.join("power1_average"))
            .or_else(|| read_num(&h.join("power1_input")))
            .map(|uw| uw as f32 / 1e6);
    }
}

/// Resolves a marketing name from the PCI id database, e.g. "Intel HD Graphics 620".
fn pci_name(device: &Path) -> Option<String> {
    let hex = |f: &str| -> Option<String> {
        let s = fs::read_to_string(device.join(f)).ok()?;
        Some(s.trim().trim_start_matches("0x").to_lowercase())
    };
    let vendor = hex("vendor")?;
    let dev = hex("device")?;
    let short_vendor = match vendor.as_str() {
        "8086" => Some("Intel"),
        "1002" => Some("AMD"),
        "10de" => Some("NVIDIA"),
        _ => None,
    };

    let db = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok());
    let found = db.and_then(|db| {
        let mut lines = db.lines();
        let vline = lines.find(|l| l.starts_with(&format!("{vendor}  ")))?;
        let vname = vline[vendor.len()..].trim().to_string();
        let model = lines
            .take_while(|l| l.starts_with('\t') || l.starts_with('#'))
            .find_map(|l| l.strip_prefix(&format!("\t{dev}  ")))?
            .trim()
            .to_string();
        Some((vname, model))
    });

    match found {
        Some((vname, model)) => {
            // "Kaby Lake-U GT2 [HD Graphics 620]" -> "HD Graphics 620"
            let model = match (model.rfind('['), model.rfind(']')) {
                (Some(a), Some(b)) if a < b => model[a + 1..b].to_string(),
                _ => model,
            };
            let v = short_vendor.map(str::to_string).unwrap_or(vname);
            Some(format!("{v} {model}"))
        }
        None => Some(format!(
            "{} GPU [{vendor}:{dev}]",
            short_vendor.unwrap_or("PCI")
        )),
    }
}
