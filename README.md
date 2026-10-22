# shoulder

Terminal system monitor (ratatui) with time-series plots for:

- **CPU**: total, or per core (`c`)
- **Memory**: RAM and swap
- **GPU load**: per engine (render/3D, compute, copy)
- **GPU memory**: VRAM, plus GTT / shared system memory
- **Video engines**: encode / decode / video-enhance
- **Temperatures**: every hwmon sensor, colored against its critical threshold
- **Processes**: CPU%, MEM%, GPU%, VID% (encode/decode) and GPU memory per process

```
cargo run --release
```

Keys: `q` quit · `c` per-core CPU · `w` cycle window (60/120/300/600 s) ·
`+`/`-` sample interval · `p`/space pause · `g`/Tab next GPU ·
`t` toggle process table · `s` cycle sort column ·
`n` normalize process CPU% by core count · `↑↓`/`jk`/PgUp/PgDn select

## GPU backends

| GPU | Source | Notes |
|-----|--------|-------|
| Intel (i915, xe), AMD (amdgpu), others with DRM fdinfo | `/proc/*/fdinfo` + sysfs/hwmon | No root needed, but only sees your own processes. Run as root for system-wide numbers. Intel uses one engine (VCS) for both encode and decode, so it shows up as "Video enc/dec". |
| NVIDIA | NVML (`libnvidia-ml.so`, loaded at runtime) | Feature `nvidia` (on by default). Use `--no-default-features` to leave it out. |

On integrated GPUs, "GPU memory" is the system RAM that GPU clients hold. It is
summed per client, so buffers shared between processes can be counted twice.

## Per-process GPU stats and permissions

CPU and memory columns work for every process without privileges. The GPU
columns on Intel/AMD come from `/proc/<pid>/fdinfo`, which the kernel only lets
you read for your own processes, so root-owned GPU users show `0` / `-`.
Either run with `sudo`, or grant the binary just that ability once:

```
sudo setcap cap_sys_ptrace+ep target/release/shoulder
```

(The capability is lost whenever the binary is rebuilt.) NVML lists all
processes without root.

GMEM% is relative to the GPU's memory pool: VRAM on discrete GPUs, system RAM
on integrated ones. Process CPU% is normalized by core count by default (100% =
all cores busy); press `n` for the htop convention (100% = one core).
