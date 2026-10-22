use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{Components, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::gpu::{EngineKind, GpuMonitor, GpuSample};
use crate::history::Series;

pub const WINDOWS: [f64; 4] = [60.0, 120.0, 300.0, 600.0];
pub const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 3000, 5000];

pub struct EngineHist {
    pub id: String,
    pub label: String,
    pub kind: EngineKind,
    pub series: Series,
}

pub struct GpuHist {
    pub id: String,
    pub engines: Vec<EngineHist>,
    pub vram: Series,
    pub shared: Series,
    pub allocated: Series,
    /// VRAM + shared bytes actually resident.
    pub resident: Series,
    pub last: GpuSample,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Cpu,
    Mem,
    Gpu,
    Video,
    GpuMem,
}

impl SortBy {
    pub fn next(self) -> Self {
        match self {
            SortBy::Cpu => SortBy::Mem,
            SortBy::Mem => SortBy::Gpu,
            SortBy::Gpu => SortBy::Video,
            SortBy::Video => SortBy::GpuMem,
            SortBy::GpuMem => SortBy::Cpu,
        }
    }
}

pub struct ProcRow {
    pub pid: u32,
    pub name: String,
    pub command: String,
    /// htop convention: 100% == one core fully busy.
    pub cpu: f64,
    pub mem: u64,
    pub mem_pct: f64,
    /// Busiest non-media engine across GPUs.
    pub gpu: f64,
    /// Busiest encode/decode/enhance engine across GPUs.
    pub video: f64,
    /// VRAM + GTT/shared bytes across GPUs.
    pub gpu_mem: u64,
    /// Share of the GPU's memory pool: VRAM, or system RAM for integrated GPUs.
    pub gpu_mem_pct: f64,
}

pub struct TempReading {
    pub label: String,
    pub temp: f32,
    pub max_seen: f32,
    pub critical: Option<f32>,
}

pub struct App {
    sys: System,
    components: Components,
    gpu: GpuMonitor,
    start: Instant,

    pub now: f64,
    pub cpu_name: String,
    pub cpu_total: Series,
    pub cpu_cores: Vec<Series>,
    pub mem_used: Series,
    pub swap_used: Series,
    pub mem_total: u64,
    pub swap_total: u64,
    pub gpus: Vec<GpuHist>,
    pub temps: Vec<TempReading>,
    pub procs: Vec<ProcRow>,

    /// Without root, GPU fdinfo of other users' processes is unreadable.
    pub is_root: bool,
    pub show_procs: bool,
    pub sort_by: SortBy,
    pub proc_sel: usize,
    /// First visible table row; updated while drawing to keep the selection on screen.
    pub proc_offset: Cell<usize>,
    pub gpu_sel: usize,
    pub window_idx: usize,
    pub interval_idx: usize,
    pub per_core: bool,
    pub paused: bool,
}

impl App {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        refresh_processes(&mut sys);
        let cpu_name = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_default();
        let ncores = sys.cpus().len();
        let mut gpu = GpuMonitor::new();
        gpu.sample(); // establish fdinfo baselines

        let mut app = Self {
            components: Components::new_with_refreshed_list(),
            gpu,
            start: Instant::now(),
            now: 0.0,
            cpu_name,
            cpu_total: Series::default(),
            cpu_cores: vec![Series::default(); ncores],
            mem_used: Series::default(),
            swap_used: Series::default(),
            mem_total: sys.total_memory(),
            swap_total: sys.total_swap(),
            gpus: Vec::new(),
            temps: Vec::new(),
            procs: Vec::new(),
            is_root: std::fs::metadata("/proc/self")
                .is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0),
            show_procs: true,
            sort_by: SortBy::Cpu,
            proc_sel: 0,
            proc_offset: Cell::new(0),
            sys,
            gpu_sel: 0,
            window_idx: 1,
            interval_idx: 2,
            per_core: false,
            paused: false,
        };
        app.sample_temps();
        app
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(INTERVALS_MS[self.interval_idx])
    }

    pub fn window(&self) -> f64 {
        WINDOWS[self.window_idx]
    }

    pub fn selected_gpu(&self) -> Option<&GpuHist> {
        self.gpus.get(self.gpu_sel)
    }

    pub fn sample(&mut self) {
        self.now = self.start.elapsed().as_secs_f64();
        let t = self.now;

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.cpu_total.push(t, self.sys.global_cpu_usage() as f64);
        for (s, c) in self.cpu_cores.iter_mut().zip(self.sys.cpus()) {
            s.push(t, c.cpu_usage() as f64);
        }
        self.mem_total = self.sys.total_memory();
        self.swap_total = self.sys.total_swap();
        self.mem_used.push(t, self.sys.used_memory() as f64);
        self.swap_used.push(t, self.sys.used_swap() as f64);

        for g in self.gpu.sample() {
            let idx = match self.gpus.iter().position(|h| h.id == g.id) {
                Some(i) => i,
                None => {
                    self.gpus.push(GpuHist {
                        id: g.id.clone(),
                        engines: Vec::new(),
                        vram: Series::default(),
                        shared: Series::default(),
                        allocated: Series::default(),
                        resident: Series::default(),
                        last: GpuSample::default(),
                    });
                    self.gpus.len() - 1
                }
            };
            let h = &mut self.gpus[idx];
            for e in &g.engines {
                let eh = match h.engines.iter().position(|x| x.id == e.id) {
                    Some(i) => &mut h.engines[i],
                    None => {
                        h.engines.push(EngineHist {
                            id: e.id.clone(),
                            label: e.label.clone(),
                            kind: e.kind,
                            series: Series::default(),
                        });
                        h.engines.last_mut().unwrap()
                    }
                };
                eh.series.push(t, e.busy);
            }
            h.engines.sort_by_key(|e| (e.kind, e.id.clone()));
            if let Some(v) = g.vram_used {
                h.vram.push(t, v as f64);
            }
            if let Some(v) = g.shared_used {
                h.shared.push(t, v as f64);
            }
            let resident = g.vram_used.unwrap_or(0) + g.shared_used.unwrap_or(0);
            h.resident.push(t, resident as f64);
            if let Some(v) = g.allocated {
                h.allocated.push(t, v as f64);
            }
            h.last = g;
        }

        self.sample_temps();
        self.sample_procs();
    }

    fn sample_procs(&mut self) {
        refresh_processes(&mut self.sys);

        // pid -> (gpu%, video%, bytes, pool%) merged across GPUs
        let mut gpu: HashMap<u32, (f64, f64, u64, f64)> = HashMap::new();
        for g in &self.gpus {
            for p in &g.last.processes {
                let pool_pct = match g.last.vram_total {
                    Some(total) if total > 0 => 100.0 * p.vram as f64 / total as f64,
                    _ => 100.0 * (p.vram + p.shared) as f64 / self.mem_total.max(1) as f64,
                };
                let e = gpu.entry(p.pid).or_default();
                e.0 = e.0.max(p.gpu);
                e.1 = e.1.max(p.media);
                e.2 += p.vram + p.shared;
                e.3 = e.3.max(pool_pct);
            }
        }

        let mem_total = self.mem_total.max(1) as f64;
        self.procs = self
            .sys
            .processes()
            .iter()
            .filter(|(_, p)| p.thread_kind().is_none())
            .map(|(pid, p)| {
                let pid = pid.as_u32();
                let (g, v, gm, gm_pct) = gpu.get(&pid).copied().unwrap_or_default();
                ProcRow {
                    pid,
                    name: p.name().to_string_lossy().into_owned(),
                    command: p
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                    cpu: p.cpu_usage() as f64,
                    mem: p.memory(),
                    mem_pct: 100.0 * p.memory() as f64 / mem_total,
                    gpu: g,
                    video: v,
                    gpu_mem: gm,
                    gpu_mem_pct: gm_pct,
                }
            })
            .collect();
        self.sort_procs();
    }

    pub fn sort_procs(&mut self) {
        let key = |r: &ProcRow| -> (f64, f64) {
            match self.sort_by {
                SortBy::Cpu => (r.cpu, r.mem as f64),
                SortBy::Mem => (r.mem as f64, r.cpu),
                SortBy::Gpu => (r.gpu, r.gpu_mem as f64),
                SortBy::Video => (r.video, r.gpu),
                SortBy::GpuMem => (r.gpu_mem as f64, r.gpu),
            }
        };
        self.procs.sort_by(|a, b| {
            key(b)
                .partial_cmp(&key(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.pid.cmp(&b.pid))
        });
        self.proc_sel = self.proc_sel.min(self.procs.len().saturating_sub(1));
    }

    pub fn move_selection(&mut self, delta: isize) {
        let max = self.procs.len().saturating_sub(1) as isize;
        self.proc_sel = (self.proc_sel as isize + delta).clamp(0, max) as usize;
    }

    fn sample_temps(&mut self) {
        self.components.refresh(false);
        let mut readings: Vec<(String, f32, Option<f32>)> = self
            .components
            .list()
            .iter()
            .filter_map(|c| Some((c.label().to_string(), c.temperature()?, c.critical())))
            .filter(|(_, t, _)| t.is_finite() && *t > 0.0)
            .collect();
        // GPUs not exposed through hwmon (NVML) still deserve a row.
        for g in &self.gpus {
            if let Some(t) = g.last.temp_c
                && g.last.driver == "nvidia"
            {
                readings.push((format!("{} (GPU)", g.last.name), t, None));
            }
        }
        for (label, temp, critical) in readings {
            match self.temps.iter_mut().find(|r| r.label == label) {
                Some(r) => {
                    r.temp = temp;
                    r.max_seen = r.max_seen.max(temp);
                    r.critical = critical;
                }
                None => self.temps.push(TempReading {
                    label,
                    temp,
                    max_seen: temp,
                    critical,
                }),
            }
        }
    }
}

fn refresh_processes(sys: &mut System) {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
}
