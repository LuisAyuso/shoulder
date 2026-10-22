use std::collections::HashMap;

use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::{Device, Nvml};

use super::{Engine, EngineKind, GpuProcess, GpuSample};

pub struct Nvidia {
    nvml: Nvml,
    count: u32,
    /// Per device: newest process-utilization timestamp already consumed.
    last_seen: Vec<u64>,
}

impl Nvidia {
    /// Returns `None` when libnvidia-ml is not installed or no device exists.
    pub fn new() -> Option<Self> {
        let nvml = Nvml::init().ok()?;
        let count = nvml.device_count().ok()?;
        (count > 0).then_some(Self {
            nvml,
            count,
            last_seen: vec![0; count as usize],
        })
    }

    pub fn sample(&mut self) -> Vec<GpuSample> {
        let (nvml, last_seen) = (&self.nvml, &mut self.last_seen);
        (0..self.count)
            .filter_map(|i| {
                let dev = nvml.device_by_index(i).ok()?;
                let processes = processes(&dev, &mut last_seen[i as usize]);
                let mut engines = Vec::new();
                let mut push = |id: &str, label: &str, kind, busy: Option<u32>| {
                    if let Some(b) = busy {
                        engines.push(Engine {
                            id: id.into(),
                            label: label.into(),
                            kind,
                            busy: b as f64,
                        });
                    }
                };
                push(
                    "gpu",
                    "Graphics",
                    EngineKind::Graphics,
                    dev.utilization_rates().ok().map(|u| u.gpu),
                );
                push(
                    "enc",
                    "Encode",
                    EngineKind::Encode,
                    dev.encoder_utilization().ok().map(|u| u.utilization),
                );
                push(
                    "dec",
                    "Decode",
                    EngineKind::Decode,
                    dev.decoder_utilization().ok().map(|u| u.utilization),
                );

                let mem = dev.memory_info().ok();
                Some(GpuSample {
                    id: format!("nvml:{i}"),
                    name: dev.name().unwrap_or_else(|_| format!("NVIDIA GPU {i}")),
                    driver: "nvidia".into(),
                    engines,
                    processes,
                    vram_used: mem.as_ref().map(|m| m.used),
                    vram_total: mem.as_ref().map(|m| m.total),
                    shared_used: None,
                    allocated: None,
                    temp_c: dev
                        .temperature(TemperatureSensor::Gpu)
                        .ok()
                        .map(|t| t as f32),
                    freq_mhz: dev.clock_info(Clock::Graphics).ok(),
                    max_freq_mhz: dev.max_clock_info(Clock::Graphics).ok(),
                    power_w: dev.power_usage().ok().map(|mw| mw as f32 / 1000.0),
                    note: None,
                })
            })
            .collect()
    }
}

fn processes(dev: &Device, last_seen: &mut u64) -> Vec<GpuProcess> {
    let mut procs: HashMap<u32, GpuProcess> = HashMap::new();
    let lists = [
        dev.running_graphics_processes(),
        dev.running_compute_processes(),
    ];
    for info in lists.into_iter().flatten().flatten() {
        let p = procs.entry(info.pid).or_insert_with(|| GpuProcess {
            pid: info.pid,
            ..Default::default()
        });
        if let UsedGpuMemory::Used(b) = info.used_gpu_memory {
            p.vram = p.vram.max(b);
        }
    }
    // Utilization samples since the previous call; keep the newest per pid.
    let mut newest: HashMap<u32, u64> = HashMap::new();
    for s in dev
        .process_utilization_stats(*last_seen)
        .unwrap_or_default()
    {
        *last_seen = (*last_seen).max(s.timestamp);
        if newest.get(&s.pid).is_some_and(|&t| t >= s.timestamp) {
            continue;
        }
        newest.insert(s.pid, s.timestamp);
        let p = procs.entry(s.pid).or_insert_with(|| GpuProcess {
            pid: s.pid,
            ..Default::default()
        });
        p.gpu = s.sm_util as f64;
        p.media = s.enc_util.max(s.dec_util) as f64;
    }
    procs.into_values().collect()
}
