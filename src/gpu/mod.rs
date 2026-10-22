//! GPU telemetry.
//!
//! Two backends feed a common [`GpuSample`]:
//! - `drm`: vendor-neutral, built on the kernel's DRM fdinfo interface
//!   (`/proc/<pid>/fdinfo/<fd>`) plus a few sysfs/hwmon extras. Works for
//!   i915, xe, amdgpu, msm, panfrost, ... without root (but only sees clients
//!   owned by the current user unless run as root).
//! - `nvidia`: NVML, loaded at runtime. Compiled with the `nvidia` feature.

mod drm;
#[cfg(feature = "nvidia")]
mod nvidia;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EngineKind {
    Graphics,
    Compute,
    Copy,
    Video,
    VideoEnhance,
    Encode,
    Decode,
    Other,
}

impl EngineKind {
    /// Media engines are plotted on their own panel.
    pub fn is_media(self) -> bool {
        matches!(
            self,
            EngineKind::Video | EngineKind::VideoEnhance | EngineKind::Encode | EngineKind::Decode
        )
    }
}

#[derive(Clone, Debug)]
pub struct Engine {
    /// Stable identifier (e.g. the fdinfo engine name).
    pub id: String,
    /// Human friendly label.
    pub label: String,
    pub kind: EngineKind,
    /// Busy percentage, 0..=100.
    pub busy: f64,
}

/// Per-process usage of one GPU.
#[derive(Clone, Debug, Default)]
pub struct GpuProcess {
    pub pid: u32,
    /// Busiest non-media engine, 0..=100.
    pub gpu: f64,
    /// Busiest media (encode/decode/enhance) engine, 0..=100.
    pub media: f64,
    pub vram: u64,
    pub shared: u64,
}

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub engines: Vec<Engine>,
    pub processes: Vec<GpuProcess>,
    /// Dedicated memory in use / installed (bytes).
    pub vram_used: Option<u64>,
    pub vram_total: Option<u64>,
    /// System memory (GTT / shared) held by GPU clients (bytes).
    pub shared_used: Option<u64>,
    /// Memory allocated by GPU clients in any region, including parts not
    /// currently resident (bytes). Only known from DRM fdinfo.
    pub allocated: Option<u64>,
    pub temp_c: Option<f32>,
    pub freq_mhz: Option<u32>,
    pub max_freq_mhz: Option<u32>,
    pub power_w: Option<f32>,
    /// Backend specific remark shown in the UI (e.g. visibility caveats).
    pub note: Option<String>,
}

pub struct GpuMonitor {
    #[cfg(feature = "nvidia")]
    nvidia: Option<nvidia::Nvidia>,
    drm: drm::DrmMonitor,
}

impl GpuMonitor {
    pub fn new() -> Self {
        #[cfg(feature = "nvidia")]
        let nvidia = nvidia::Nvidia::new();
        #[cfg(feature = "nvidia")]
        let skip_nvidia_drm = nvidia.is_some();
        #[cfg(not(feature = "nvidia"))]
        let skip_nvidia_drm = false;

        Self {
            #[cfg(feature = "nvidia")]
            nvidia,
            drm: drm::DrmMonitor::new(skip_nvidia_drm),
        }
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        let mut out = Vec::new();
        #[cfg(feature = "nvidia")]
        if let Some(nv) = &mut self.nvidia {
            out.extend(nv.sample());
        }
        out.extend(self.drm.sample());
        out
    }
}
