// SPDX-License-Identifier: GPL-3.0-or-later
//! Dashboard sensor readings, ported from agent/src/core/SensorSource.cpp,
//! AmdGpu.cpp and Gpus.cpp.
//!
//! Everything reads local /proc and /sys or shells out to nvidia-smi;
//! no root, no daemon. The low-level readers take explicit paths so the tests
//! can point them at a fixture tree and never require a real GPU or disk.
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

// First line of a sysfs attribute, or empty. Absence is normal here: not every
// card exposes every attribute, and the caller decides what is required.
fn read_attr(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

fn read_number(path: &Path, scale: f64) -> f64 {
    let s = read_attr(path);
    if s.is_empty() {
        return -1.0;
    }
    match s.parse::<f64>() {
        Ok(v) => v * scale,
        Err(_) => -1.0,
    }
}

fn parse_u64(s: &str) -> u64 {
    s.trim().parse().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// GPU data model (Gpus.cpp)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GpuInfo {
    /// Stable across reboots and across cards being added: an NVIDIA UUID or a
    /// PCI bus address, never an index.
    pub id: String,
    pub name: String,
    pub source: String, // "nvidia-smi" or "amdgpu"
    pub util_pct: f64,  // -1 unknown
    pub temp_c: f64,
    pub mem_used_gib: f64,
    pub mem_total_gib: f64,
    pub power_w: f64,
}

impl GpuInfo {
    #[allow(dead_code)]
    pub fn valid(&self) -> bool {
        !self.id.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct AmdGpuSample {
    pub card: String,       // "card2"
    pub bus_address: String, // "0000:72:00.0", empty when not resolvable
    pub device_id: String,  // "13c0", from the PCI device id
    pub busy_pct: f64,
    pub vram_used_gib: f64,
    pub vram_total_gib: f64,
    pub temp_c: f64,
    pub power_w: f64,
}

impl AmdGpuSample {
    #[allow(dead_code)]
    pub fn valid(&self) -> bool {
        !self.card.is_empty()
    }
}

// nvidia-smi prints "[N/A]" for a figure the card does not report, which is
// not zero and must not be shown as zero.
fn num(field: &str) -> f64 {
    let s = field.trim();
    if s.is_empty() || s.starts_with('[') {
        return -1.0;
    }
    s.parse::<f64>().unwrap_or(-1.0)
}

/// Parses the output of nvidia-smi --format=csv,noheader,nounits with
/// --query-gpu=uuid,name,temperature.gpu,utilization.gpu,memory.used,
/// memory.total,power.draw. One line per GPU. "[N/A]" becomes -1.
pub fn parse_nvidia_smi(out: &str) -> Vec<GpuInfo> {
    let mut list = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 7 {
            continue;
        }
        let uuid = fields[0].trim();
        if uuid.is_empty() {
            continue;
        }
        // uuid, name, then five numbers. A card name with a comma in it would
        // split into more fields, so the name is whatever sits between the
        // uuid and the last five columns rather than field 1 alone.
        let name = fields[1..fields.len() - 5].join(",").trim().to_string();
        let used_mib = num(fields[fields.len() - 3]);
        let total_mib = num(fields[fields.len() - 2]);
        list.push(GpuInfo {
            id: format!("nvidia:{uuid}"),
            name,
            source: "nvidia-smi".to_string(),
            temp_c: num(fields[fields.len() - 5]),
            util_pct: num(fields[fields.len() - 4]),
            mem_used_gib: if used_mib >= 0.0 { used_mib / 1024.0 } else { -1.0 },
            mem_total_gib: if total_mib >= 0.0 { total_mib / 1024.0 } else { -1.0 },
            power_w: num(fields[fields.len() - 1]),
        });
    }
    list
}

pub fn from_amd_sample(s: &AmdGpuSample, name: &str) -> GpuInfo {
    // The bus address is the stable identifier. "card0" is a kernel
    // enumeration order, and adding a card can renumber it.
    let id = if s.bus_address.is_empty() {
        s.card.clone()
    } else {
        s.bus_address.clone()
    };
    GpuInfo {
        id: format!("amdgpu:{id}"),
        name: name.to_string(),
        source: "amdgpu".to_string(),
        util_pct: s.busy_pct,
        temp_c: s.temp_c,
        mem_used_gib: s.vram_used_gib,
        mem_total_gib: s.vram_total_gib,
        power_w: s.power_w,
    }
}

/// The GPUs the dashboard should show, in detection order.
///
/// An empty selection means automatic: one card, the one with the most VRAM.
/// Ids that are no longer present are dropped. If that drops all of them, this
/// falls back to automatic rather than to an empty dashboard.
pub fn select_gpus(all: &[GpuInfo], wanted: &[String]) -> Vec<GpuInfo> {
    if all.is_empty() {
        return Vec::new();
    }
    if !wanted.is_empty() {
        let mut out = Vec::new();
        // Walking the detected list rather than the wanted list keeps the
        // order the machine reports and quietly handles an id listed twice.
        for g in all {
            if wanted.iter().any(|w| w == &g.id) {
                out.push(g.clone());
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    let mut best: Option<&GpuInfo> = None;
    for g in all {
        if best.is_none() || g.mem_total_gib > best.unwrap().mem_total_gib {
            best = Some(g);
        }
    }
    vec![best.unwrap().clone()]
}

// ---------------------------------------------------------------------------
// amdgpu sysfs (AmdGpu.cpp)
// ---------------------------------------------------------------------------

fn is_card_dir(name: &str) -> bool {
    match name.strip_prefix("card") {
        Some(d) => !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

fn is_bus_addr(s: &str) -> bool {
    let b = s.as_bytes();
    let f = |c: u8| c.is_ascii_hexdigit();
    b.len() == 12
        && f(b[0]) && f(b[1]) && f(b[2]) && f(b[3]) && b[4] == b':'
        && f(b[5]) && f(b[6]) && b[7] == b':'
        && f(b[8]) && f(b[9]) && b[10] == b'.'
        && f(b[11])
}

// The hwmon directory that belongs to this card. It is under the card's own
// device directory, so there is no need to search the global hwmon list.
fn find_amdgpu_hwmon(device_dir: &Path) -> Option<PathBuf> {
    let hw = device_dir.join("hwmon");
    let Ok(entries) = std::fs::read_dir(&hw) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if read_attr(&path.join("name")) == "amdgpu" {
            return Some(path);
        }
    }
    None
}

/// Every card whose PCI vendor is AMD. Cards without the amdgpu attributes
/// (an old radeon-driver part, or a card that is powered off) are skipped.
pub fn read_amd_gpus(drm_root: &Path) -> Vec<AmdGpuSample> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(drm_root) else {
        return out;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        if !is_card_dir(&name) {
            continue; // card2-DP-5 and friends are connectors, not cards
        }
        let dev = drm_root.join(&name).join("device");
        if read_attr(&dev.join("vendor")).to_lowercase() != "0x1002" {
            continue;
        }
        let busy_pct = read_number(&dev.join("gpu_busy_percent"), 1.0);
        let vram_used_gib = read_number(&dev.join("mem_info_vram_used"), 1.0 / GIB);
        let vram_total_gib = read_number(&dev.join("mem_info_vram_total"), 1.0 / GIB);
        // A card with neither a load figure nor a VRAM figure is not one the
        // amdgpu driver is running; it has nothing worth showing.
        if busy_pct < 0.0 && vram_total_gib < 0.0 {
            continue;
        }

        let device_id = read_attr(&dev.join("device")).to_lowercase();
        let device_id = if let Some(stripped) = device_id.strip_prefix("0x") {
            stripped.to_string()
        } else {
            device_id
        };

        let resolved = std::fs::canonicalize(&dev).unwrap_or_else(|_| dev.clone());
        let base = resolved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bus_address = if is_bus_addr(&base) { base } else { String::new() };

        let mut temp_c = -1.0;
        let mut power_w = -1.0;
        if let Some(hw) = find_amdgpu_hwmon(&dev) {
            temp_c = read_number(&hw.join("temp1_input"), 1.0 / 1000.0); // millidegrees
            // Newer kernels report power1_average; older ones power1_input. Both
            // are microwatts.
            power_w = read_number(&hw.join("power1_average"), 1.0 / 1e6);
            if power_w < 0.0 {
                power_w = read_number(&hw.join("power1_input"), 1.0 / 1e6);
            }
        }

        out.push(AmdGpuSample {
            card: name,
            bus_address,
            device_id,
            busy_pct,
            vram_used_gib,
            vram_total_gib,
            temp_c,
            power_w,
        });
    }
    out
}

/// The card to show when there are several: the one with the most VRAM.
#[allow(dead_code)] // ported C++ API; exercised by tests, wired when the GPU picker lands
pub fn pick_amd_gpu(cards: &[AmdGpuSample]) -> Option<&AmdGpuSample> {
    let mut best: Option<&AmdGpuSample> = None;
    for c in cards {
        if best.is_none() || c.vram_total_gib > best.unwrap().vram_total_gib {
            best = Some(c);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Network (SensorSource.cpp)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct NetInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub up: bool,        // operstate "up", or "unknown" with a carrier
    pub wireless: bool,  // has /sys/class/net/<name>/wireless
    pub is_virtual: bool,
}

// Interfaces whose traffic is a copy of something else's: bridges, container
// veths, tunnels and loopback. Counting them would double-count traffic.
fn is_virtual_interface(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "lo", "docker", "br-", "virbr", "veth", "tun", "tap", "vmnet", "wg",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Every interface the kernel lists, for the picker.
pub fn enumerate_interfaces() -> Vec<NetInterface> {
    enumerate_interfaces_from(
        Path::new("/proc/net/dev"),
        Path::new("/sys/class/net"),
    )
}

pub fn enumerate_interfaces_from(proc_net_dev: &Path, sys_class_net: &Path) -> Vec<NetInterface> {
    let mut out = Vec::new();
    let Ok(text) = std::fs::read_to_string(proc_net_dev) else {
        return out;
    };
    for line in text.lines() {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let cols: Vec<&str> = line[colon + 1..].split(' ').filter(|s| !s.is_empty()).collect();
        if cols.len() < 9 {
            continue;
        }
        let name = line[..colon].trim().to_string();
        let is_virtual = is_virtual_interface(&name);
        let state = read_trimmed(&sys_class_net.join(&name).join("operstate"));
        let up = if state == "up" {
            true
        } else if state == "unknown" {
            // Some drivers (ppp, some USB modems) never report a state; the
            // carrier file is the better answer for those.
            read_trimmed(&sys_class_net.join(&name).join("carrier")) == "1"
        } else {
            false
        };
        out.push(NetInterface {
            wireless: sys_class_net.join(&name).join("wireless").exists(),
            name,
            rx_bytes: parse_u64(cols[0]),
            tx_bytes: parse_u64(cols[8]),
            up,
            is_virtual,
        });
    }
    out
}

#[derive(Default)]
pub struct NetworkState {
    pub selection: String,
    pub name: String,
    pub rx: u64,
    pub tx: u64,
    pub last_ms: i64,
}

fn pick_interface<'a>(all: &'a [NetInterface], selection: &str) -> Option<&'a NetInterface> {
    // The owner's choice wins whenever that interface exists, virtual or not.
    if !selection.is_empty() {
        if let Some(n) = all.iter().find(|n| n.name == selection) {
            return Some(n);
        }
    }
    // Otherwise the busiest physical interface whose link is up.
    let mut best_pick: Option<&NetInterface> = None;
    let mut best = 0u64;
    let mut best_up = false;
    for n in all {
        if n.is_virtual {
            continue;
        }
        let total = n.rx_bytes + n.tx_bytes;
        if best_pick.is_none() || (n.up && !best_up) || (n.up == best_up && total > best) {
            best_pick = Some(n);
            best = total;
            best_up = n.up;
        }
    }
    best_pick
}

/// Reads the busiest physical interface and its per-second rate. Returns
/// (interface name, rx MB/s, tx MB/s); rates are -1 when there is no delta.
pub fn read_network(
    proc_net_dev: &Path,
    sys_class_net: &Path,
    now: i64,
    state: &mut NetworkState,
) -> (String, f64, f64) {
    let all = enumerate_interfaces_from(proc_net_dev, sys_class_net);
    let dt = if state.last_ms > 0 {
        (now - state.last_ms) as f64 / 1000.0
    } else {
        0.0
    };
    state.last_ms = now;

    let Some(pick) = pick_interface(&all, &state.selection) else {
        return (String::new(), -1.0, -1.0);
    };

    let mut rx = -1.0;
    let mut tx = -1.0;
    // A different interface, or the first sample, has no delta to report.
    if dt > 0.0 && pick.name == state.name && pick.rx_bytes >= state.rx && pick.tx_bytes >= state.tx {
        rx = (pick.rx_bytes - state.rx) as f64 / dt / MIB;
        tx = (pick.tx_bytes - state.tx) as f64 / dt / MIB;
    }
    let name = pick.name.clone();
    state.name = pick.name.clone();
    state.rx = pick.rx_bytes;
    state.tx = pick.tx_bytes;
    (name, rx, tx)
}

// ---------------------------------------------------------------------------
// Disk (SensorSource.cpp)
// ---------------------------------------------------------------------------

fn is_whole_disk(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("nvme") {
        // nvme\d+n\d+
        return match rest.split_once('n') {
            Some((a, b)) => {
                !a.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && !b.is_empty()
                    && b.chars().all(|c| c.is_ascii_digit())
            }
            None => false,
        };
    }
    if name.starts_with("sd") || name.starts_with("vd") {
        let rest = &name[2..];
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase());
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

#[derive(Default)]
pub struct DiskState {
    pub name: String,
    pub read: u64,
    pub write: u64,
    pub last_ms: i64,
}

/// Reads the busiest whole device and its per-second throughput. Returns
/// (device name, read MB/s, write MB/s).
pub fn read_disk(
    diskstats: &Path,
    now: i64,
    state: &mut DiskState,
) -> (String, f64, f64) {
    let Ok(text) = std::fs::read_to_string(diskstats) else {
        return (String::new(), -1.0, -1.0);
    };
    let dt = if state.last_ms > 0 {
        (now - state.last_ms) as f64 / 1000.0
    } else {
        0.0
    };
    state.last_ms = now;

    let mut best_name = String::new();
    let mut best_read = 0u64;
    let mut best_write = 0u64;
    let mut best_total = 0u64;
    for line in text.lines() {
        let cols: Vec<&str> = line.split(' ').filter(|s| !s.is_empty()).collect();
        if cols.len() < 10 {
            continue;
        }
        let name = cols[2];
        if !is_whole_disk(name) {
            continue;
        }
        // Fields 6 and 10 are sectors read and written, 512 bytes each.
        let rd = parse_u64(cols[5]);
        let wr = parse_u64(cols[9]);
        if rd + wr > best_total {
            best_total = rd + wr;
            best_name = name.to_string();
            best_read = rd;
            best_write = wr;
        }
    }
    if best_name.is_empty() {
        return (String::new(), -1.0, -1.0);
    }

    let mut read_mbs = -1.0;
    let mut write_mbs = -1.0;
    if dt > 0.0 && best_name == state.name && best_read >= state.read && best_write >= state.write {
        read_mbs = (best_read - state.read) as f64 * 512.0 / dt / MIB;
        write_mbs = (best_write - state.write) as f64 * 512.0 / dt / MIB;
    }
    state.name = best_name.clone();
    state.read = best_read;
    state.write = best_write;
    (best_name, read_mbs, write_mbs)
}

// ---------------------------------------------------------------------------
// CPU load / memory / uptime / topology (SensorSource.cpp)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct LoadState {
    last_idle: u64,
    last_total: u64,
    have: bool,
}

/// /proc/stat load delta since the last call. -1 on the first call or on read
/// failure.
pub fn read_cpu_load(proc_stat: &Path, state: &mut LoadState) -> f64 {
    let Ok(text) = std::fs::read_to_string(proc_stat) else {
        return -1.0;
    };
    let Some(line) = text.lines().next() else {
        return -1.0;
    };
    let mut parts = line.split_whitespace();
    if parts.next() != Some("cpu") {
        return -1.0;
    }
    let vals: Vec<u64> = parts.take(7).map(|s| s.parse().unwrap_or(0)).collect();
    if vals.len() < 7 {
        return -1.0;
    }
    let idle = vals[3] + vals[4]; // idle + iowait
    let total: u64 = vals.iter().sum();

    let mut load = -1.0;
    if state.have {
        let dt = total - state.last_total;
        let di = idle - state.last_idle;
        if dt > 0 {
            load = 100.0 * ((dt - di) as f64 / dt as f64);
        }
    }
    state.last_idle = idle;
    state.last_total = total;
    state.have = true;
    load
}

pub fn read_cpu_temp(path: &Path) -> f64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return -1.0;
    };
    text.trim().parse::<f64>().map(|v| v / 1000.0).unwrap_or(-1.0)
}

/// Returns (used GiB, total GiB, percent).
pub fn read_memory(meminfo: &Path) -> (f64, f64, f64) {
    let Ok(text) = std::fs::read_to_string(meminfo) else {
        return (0.0, 0.0, -1.0);
    };
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let key = it.next();
        let val = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        match key {
            Some("MemTotal:") => total = val,
            Some("MemAvailable:") => avail = val,
            _ => {}
        }
    }
    if total > 0 {
        let total_gib = total as f64 / 1048576.0; // kB -> GiB
        let used_gib = (total - avail) as f64 / 1048576.0;
        let pct = 100.0 * ((total - avail) as f64 / total as f64);
        (used_gib, total_gib, pct)
    } else {
        (0.0, 0.0, -1.0)
    }
}

pub fn read_uptime(uptime: &Path) -> i64 {
    let Ok(text) = std::fs::read_to_string(uptime) else {
        return -1;
    };
    let secs = text.split_whitespace().next().and_then(|s| s.parse::<f64>().ok());
    match secs {
        Some(v) => v as i64,
        None => -1,
    }
}

/// The root filesystem, not the busiest block device. Returns percent used.
pub fn read_disk_usage(root: &Path) -> f64 {
    let Some(path) = root.to_str() else {
        return -1.0;
    };
    let Ok(cpath) = CString::new(path) else {
        return -1.0;
    };
    let mut vfs = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut vfs) } != 0 || vfs.f_blocks == 0 {
        return -1.0;
    }
    disk_usage_from(vfs)
}

/// Compute used-percent from a [`libc::statvfs`]. Pure and deterministic, so
/// tests can assert an exact result without depending on a live filesystem.
pub fn disk_usage_from(vfs: libc::statvfs) -> f64 {
    let total = vfs.f_blocks as f64;
    // f_bavail rather than f_bfree: the blocks reserved for root are not space
    // this user can fill, so counting them as free overstates what is left.
    let avail = vfs.f_bavail as f64;
    100.0 * (total - avail) / total
}

/// Returns (cores, threads). Returns -1 for the unknown half.
pub fn read_cpu_topology(cpuinfo: &Path) -> (i32, i32) {
    let Ok(text) = std::fs::read_to_string(cpuinfo) else {
        return (-1, -1);
    };
    let mut threads = 0;
    let mut cores: HashSet<String> = HashSet::new();
    let mut phys = String::new();
    for line in text.lines() {
        if line.starts_with("processor") {
            threads += 1;
        } else if line.starts_with("physical id") {
            phys = line.split(':').nth(1).map(|s| s.trim().to_string()).unwrap_or_default();
        } else if line.starts_with("core id") {
            let core = line.split(':').nth(1).map(|s| s.trim().to_string()).unwrap_or_default();
            cores.insert(format!("{phys}:{core}"));
        }
    }
    let threads = if threads > 0 { threads } else { -1 };
    // Architectures whose cpuinfo has no core id (arm64, among others) leave
    // this at -1 rather than claiming one core per thread.
    let cores = if cores.is_empty() { -1 } else { cores.len() as i32 };
    (cores, threads)
}

// ---------------------------------------------------------------------------
// Fan / package power (SensorSource.cpp)
// ---------------------------------------------------------------------------

/// The fastest fan any hwmon chip reports. Returns (rpm, chip name).
pub fn read_fan(hwmon: &Path) -> (f64, String) {
    let mut best = -1.0;
    let mut chip = String::new();
    let Ok(entries) = std::fs::read_dir(hwmon) else {
        return (best, chip);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = read_attr(&dir.join("name"));
        for i in 1..=12 {
            let path = dir.join(format!("fan{i}_input"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(rpm) = text.trim().parse::<f64>() {
                if rpm > best {
                    best = rpm;
                    chip = name.clone();
                }
            }
        }
    }
    (best, chip)
}

#[derive(Default)]
pub struct PowerState {
    pub resolved: bool,
    pub path: String,
    pub uj: u64,
    pub last_ms: i64,
}

/// powercap counts cumulative microjoules, so power is its derivative. Returns
/// watts, or -1 when there is no readable counter or no delta yet.
pub fn read_package_power(powercap: &Path, now: i64, state: &mut PowerState) -> f64 {
    if !state.resolved {
        state.resolved = true;
        let Ok(entries) = std::fs::read_dir(powercap) else {
            return -1.0;
        };
        for entry in entries.flatten() {
            let p = entry.path().join("energy_uj");
            // Readable is the operative word. These counters have been root
            // only on mainstream kernels since the power side channel papers.
            if std::fs::File::open(&p).is_ok() {
                state.path = p.to_string_lossy().into_owned();
                break;
            }
        }
    }
    if state.path.is_empty() {
        return -1.0;
    }
    let Ok(text) = std::fs::read_to_string(&state.path) else {
        return -1.0;
    };
    let Ok(uj) = text.trim().parse::<u64>() else {
        return -1.0;
    };
    let dt = if state.last_ms > 0 {
        (now - state.last_ms) as f64 / 1000.0
    } else {
        0.0
    };
    // The counter wraps at its own maximum. A decrease is a wrap, not a
    // machine that consumed negative power for a second.
    let mut watts = -1.0;
    if dt > 0.0 && uj >= state.uj {
        watts = (uj - state.uj) as f64 / 1e6 / dt;
    }
    state.uj = uj;
    state.last_ms = now;
    watts
}

// ---------------------------------------------------------------------------
// systemd / host identity
// ---------------------------------------------------------------------------

/// Parses `systemctl --failed --no-legend --plain --no-pager` output into the
/// list of failed unit names.
pub fn parse_failed_units(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let unit = line.trim().split_whitespace().next().unwrap_or("").to_string();
        if !unit.is_empty() {
            names.push(unit);
        }
    }
    names
}

fn failed_units() -> (i32, Vec<String>) {
    let Ok(out) = std::process::Command::new("systemctl")
        .args(["--failed", "--no-legend", "--plain", "--no-pager"])
        .output()
    else {
        return (-1, Vec::new());
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // Keep the last known figure rather than claiming zero on a total failure.
    if !out.status.success() && text.trim().is_empty() {
        return (-1, Vec::new());
    }
    let names = parse_failed_units(&text);
    (names.len() as i32, names)
}

fn host_name() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub cpu_load_pct: f64,
    pub cpu_temp_c: f64,
    pub ram_used_gib: f64,
    pub ram_total_gib: f64,
    pub ram_pct: f64,
    pub gpus: Vec<GpuInfo>,
    pub gpu_ids_shown: Vec<String>,
    pub gpu_util_pct: f64,
    pub gpu_temp_c: f64,
    pub gpu_mem_used_gib: f64,
    pub gpu_mem_total_gib: f64,
    pub gpu_name: String,
    pub gpu_ok: bool,
    pub gpu_source: String,
    pub gpu_power_w: f64,
    pub net_interface: String,
    pub net_rx_mbs: f64,
    pub net_tx_mbs: f64,
    pub disk_device: String,
    pub disk_read_mbs: f64,
    pub disk_write_mbs: f64,
    pub failed_units: i32,
    pub failed_unit_names: Vec<String>,
    pub host_name: String,
    pub uptime_sec: i64,
    pub cpu_cores: i32,
    pub cpu_threads: i32,
    pub disk_used_pct: f64,
    pub fan_rpm: f64,
    pub fan_chip: String,
    pub package_watts: f64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            cpu_load_pct: -1.0,
            cpu_temp_c: -1.0,
            ram_used_gib: 0.0,
            ram_total_gib: 0.0,
            ram_pct: -1.0,
            gpus: Vec::new(),
            gpu_ids_shown: Vec::new(),
            gpu_util_pct: -1.0,
            gpu_temp_c: -1.0,
            gpu_mem_used_gib: 0.0,
            gpu_mem_total_gib: 0.0,
            gpu_name: String::new(),
            gpu_ok: false,
            gpu_source: String::new(),
            gpu_power_w: -1.0,
            net_interface: String::new(),
            net_rx_mbs: -1.0,
            net_tx_mbs: -1.0,
            disk_device: String::new(),
            disk_read_mbs: -1.0,
            disk_write_mbs: -1.0,
            failed_units: -1,
            failed_unit_names: Vec::new(),
            host_name: String::new(),
            uptime_sec: -1,
            cpu_cores: -1,
            cpu_threads: -1,
            disk_used_pct: -1.0,
            fan_rpm: -1.0,
            fan_chip: String::new(),
            package_watts: -1.0,
        }
    }
}

pub fn gpu_json(g: &GpuInfo) -> Value {
    json!({
        "id": g.id, "name": g.name, "source": g.source, "utilPct": g.util_pct,
        "tempC": g.temp_c, "memUsedGiB": g.mem_used_gib, "memTotalGiB": g.mem_total_gib,
        "powerW": g.power_w,
    })
}

/// The full dashboard snapshot, matching Api::sensorSnapshot(). The MPRIS and
/// notification fields are the A4 media/notifications subsystem, which is not
/// this module's business; they are emitted as neutral placeholders so the
/// JSON contract stays stable across the migration.
#[allow(dead_code)] // used by the snapshot transport that follows the stream toggle
pub fn snapshot_json(snap: &Snapshot) -> Value {
    let gpus: Vec<Value> = snap.gpus.iter().map(gpu_json).collect();
    json!({
        "notifications": [],
        "notifyActive": false,
        "notifyError": "",
        "nowPlaying": {
            "valid": false, "player": "", "status": "", "title": "", "artist": "",
            "album": "", "positionUs": 0, "lengthUs": 0,
        },
        "sessionBusOk": false,
        "gpus": gpus,
        "gpuIdsShown": snap.gpu_ids_shown,
        "cpuLoadPct": snap.cpu_load_pct,
        "cpuTempC": snap.cpu_temp_c,
        "ramUsedGiB": snap.ram_used_gib,
        "ramTotalGiB": snap.ram_total_gib,
        "ramPct": snap.ram_pct,
        "gpuUtilPct": snap.gpu_util_pct,
        "gpuTempC": snap.gpu_temp_c,
        "gpuMemUsedGiB": snap.gpu_mem_used_gib,
        "gpuMemTotalGiB": snap.gpu_mem_total_gib,
        "gpuName": snap.gpu_name,
        "gpuOk": snap.gpu_ok,
        "gpuSource": snap.gpu_source,
        "gpuPowerW": snap.gpu_power_w,
        "netInterface": snap.net_interface,
        "netRxMBs": snap.net_rx_mbs,
        "netTxMBs": snap.net_tx_mbs,
        "diskDevice": snap.disk_device,
        "diskReadMBs": snap.disk_read_mbs,
        "diskWriteMBs": snap.disk_write_mbs,
        "hostName": snap.host_name,
        "uptimeSec": snap.uptime_sec,
        "cpuCores": snap.cpu_cores,
        "cpuThreads": snap.cpu_threads,
        "diskUsedPct": snap.disk_used_pct,
        "fanRpm": snap.fan_rpm,
        "fanChip": snap.fan_chip,
        "packageWatts": snap.package_watts,
        "failedUnits": snap.failed_units,
        "failedUnitNames": snap.failed_unit_names,
    })
}

// ---------------------------------------------------------------------------
// SensorSource: owns the per-second baselines and the selection state
// ---------------------------------------------------------------------------

fn nvidia_query_args() -> Vec<String> {
    vec![
        "--query-gpu=uuid,name,temperature.gpu,utilization.gpu,memory.used,memory.total,power.draw"
            .to_string(),
        "--format=csv,noheader,nounits".to_string(),
    ]
}

fn run_nvidia_smi() -> Option<Vec<GpuInfo>> {
    let out = std::process::Command::new("nvidia-smi")
        .args(nvidia_query_args())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout)))
}

fn find_cpu_temp_input(hwmon: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(hwmon) else {
        return String::new();
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = read_attr(&dir.join("name"));
        if name != "k10temp" && name != "coretemp" {
            continue;
        }
        // Prefer a label of Tctl/Tdie/Package; else temp1_input.
        for i in 1..=8 {
            let label = read_attr(&dir.join(format!("temp{i}_label")));
            let input = dir.join(format!("temp{i}_input"));
            if !input.exists() {
                continue;
            }
            if label.contains("Tctl") || label.contains("Tdie") || label.contains("Package") || i == 1 {
                return input.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

fn quoted_fields(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut field = String::new();
            for cc in chars.by_ref() {
                if cc == '"' {
                    break;
                }
                field.push(cc);
            }
            out.push(field);
        }
    }
    out
}

fn apply_gpus(all: &[GpuInfo], selection: &[String], snap: &mut Snapshot) {
    // NVIDIA first: on a machine that has both it is nearly always the
    // discrete card and the one the user means by "the GPU".
    snap.gpus.clear();
    snap.gpus.extend(all.iter().cloned());

    let shown = select_gpus(all, selection);
    snap.gpu_ids_shown = shown.iter().map(|g| g.id.clone()).collect();

    if shown.is_empty() {
        snap.gpu_ok = false;
        snap.gpu_name.clear();
        snap.gpu_source.clear();
        snap.gpu_util_pct = -1.0;
        snap.gpu_temp_c = -1.0;
        snap.gpu_power_w = -1.0;
        snap.gpu_mem_used_gib = 0.0;
        snap.gpu_mem_total_gib = 0.0;
        return;
    }
    let g = &shown[0];
    snap.gpu_name = g.name.clone();
    snap.gpu_source = g.source.clone();
    snap.gpu_util_pct = g.util_pct;
    snap.gpu_temp_c = g.temp_c;
    // The flat pair has always meant "0 when unknown".
    snap.gpu_mem_used_gib = if g.mem_used_gib >= 0.0 { g.mem_used_gib } else { 0.0 };
    snap.gpu_mem_total_gib = if g.mem_total_gib >= 0.0 { g.mem_total_gib } else { 0.0 };
    snap.gpu_power_w = g.power_w;
    snap.gpu_ok = true;
}

pub struct SensorSource {
    load: LoadState,
    net: NetworkState,
    disk: DiskState,
    power: PowerState,
    cpu_temp_path: String,
    cpu_cores: i32,
    cpu_threads: i32,
    gpu_selection: Vec<String>,
    nvidia_failed: bool,
    nvidia_gpus: Vec<GpuInfo>,
    amd_names: HashMap<String, String>,
    snap: Snapshot,
    #[allow(dead_code)]
    streaming: bool,
}

impl SensorSource {
    pub fn new() -> Self {
        Self {
            load: LoadState::default(),
            net: NetworkState::default(),
            disk: DiskState::default(),
            power: PowerState::default(),
            cpu_temp_path: find_cpu_temp_input(Path::new("/sys/class/hwmon")),
            cpu_cores: -1,
            cpu_threads: -1,
            gpu_selection: Vec::new(),
            nvidia_failed: false,
            nvidia_gpus: Vec::new(),
            amd_names: HashMap::new(),
            snap: Snapshot::default(),
            streaming: false,
        }
    }

    pub fn latest(&self) -> &Snapshot {
        &self.snap
    }

    pub fn net_selection(&self) -> &str {
        &self.net.selection
    }

    pub fn set_net_selection(&mut self, name: String) {
        self.net.selection = name;
    }

    pub fn gpu_selection(&self) -> &[String] {
        &self.gpu_selection
    }

    pub fn set_gpu_selection(&mut self, ids: Vec<String>) {
        self.gpu_selection = ids;
    }

    /// This Rust backend answers one request per socket line, so there is no
    /// event-broadcast channel to push snapshots on. Setting streaming on does
    /// the immediate poll the C++ start() does and records the state; the
    /// periodic delivery is a later transport concern.
    pub fn set_streaming(&mut self, enabled: bool, _interval_ms: u32) {
        self.streaming = enabled;
        if enabled {
            self.poll();
        }
    }

    fn amd_gpu_name(&mut self, g: &AmdGpuSample) -> String {
        let key = if g.bus_address.is_empty() { g.card.clone() } else { g.bus_address.clone() };
        if let Some(name) = self.amd_names.get(&key) {
            return name.clone();
        }
        let mut name = String::new();
        if !g.bus_address.is_empty() {
            if let Ok(out) = std::process::Command::new("lspci")
                .args(["-s", &g.bus_address, "-mm"])
                .output()
            {
                if out.status.success() {
                    let fields = quoted_fields(&String::from_utf8_lossy(&out.stdout));
                    // Fields are quoted: slot "class" "vendor" "device" ...
                    if fields.len() >= 3 && !fields[2].starts_with("Device ") {
                        name = fields[2].clone();
                    }
                }
            }
        }
        if name.is_empty() {
            let id = if g.device_id.is_empty() { g.card.clone() } else { g.device_id.clone() };
            name = format!("AMD Radeon ({id})");
        }
        self.amd_names.insert(key, name.clone());
        name
    }

    fn read_amd_gpu_list(&mut self) -> Vec<GpuInfo> {
        let samples = read_amd_gpus(Path::new("/sys/class/drm"));
        samples
            .iter()
            .map(|s| {
                let name = self.amd_gpu_name(s);
                from_amd_sample(s, &name)
            })
            .collect()
    }

    /// A one-shot enumeration for the settings UI. Blocks on nvidia-smi, so it
    /// is for a click, not for the poll loop.
    pub fn enumerate_gpus(&mut self) -> Vec<GpuInfo> {
        if !self.nvidia_failed {
            match run_nvidia_smi() {
                Some(gpus) => {
                    self.nvidia_gpus = gpus;
                    self.nvidia_failed = false;
                }
                None => {
                    self.nvidia_failed = true;
                    self.nvidia_gpus.clear();
                }
            }
        }
        let mut all = self.nvidia_gpus.clone();
        all.extend(self.read_amd_gpu_list());
        all
    }

    pub fn poll(&mut self) -> Snapshot {
        let mut s = Snapshot::default();

        s.cpu_load_pct = read_cpu_load(Path::new("/proc/stat"), &mut self.load);
        s.cpu_temp_c = if self.cpu_temp_path.is_empty() {
            -1.0
        } else {
            read_cpu_temp(Path::new(&self.cpu_temp_path))
        };
        let (used, total, pct) = read_memory(Path::new("/proc/meminfo"));
        s.ram_used_gib = used;
        s.ram_total_gib = total;
        s.ram_pct = pct;

        let (iface, rx, tx) = read_network(
            Path::new("/proc/net/dev"),
            Path::new("/sys/class/net"),
            now_ms(),
            &mut self.net,
        );
        s.net_interface = iface;
        s.net_rx_mbs = rx;
        s.net_tx_mbs = tx;

        let (dev, rd, wr) = read_disk(Path::new("/proc/diskstats"), now_ms(), &mut self.disk);
        s.disk_device = dev;
        s.disk_read_mbs = rd;
        s.disk_write_mbs = wr;

        s.uptime_sec = read_uptime(Path::new("/proc/uptime"));
        s.disk_used_pct = read_disk_usage(Path::new("/"));

        let (fan, chip) = read_fan(Path::new("/sys/class/hwmon"));
        if fan >= 0.0 {
            s.fan_rpm = fan;
            s.fan_chip = chip;
        }
        s.package_watts = read_package_power(Path::new("/sys/class/powercap"), now_ms(), &mut self.power);

        // Topology does not change while the process runs, so it is read once.
        if self.cpu_threads < 0 {
            let (cores, threads) = read_cpu_topology(Path::new("/proc/cpuinfo"));
            self.cpu_cores = cores;
            self.cpu_threads = threads;
        }
        s.cpu_cores = self.cpu_cores;
        s.cpu_threads = self.cpu_threads;

        if s.host_name.is_empty() {
            s.host_name = host_name();
        }

        // Both vendors are read every poll. nvidia-smi is sync here, so its
        // result folds into this snapshot rather than the next one.
        let mut all = self.nvidia_gpus.clone();
        all.extend(self.read_amd_gpu_list());
        apply_gpus(&all, &self.gpu_selection, &mut s);

        let (count, names) = failed_units();
        s.failed_units = count;
        s.failed_unit_names = names;

        self.snap = s.clone();
        s
    }
}

impl Default for SensorSource {
    fn default() -> Self {
        Self::new()
    }
}

pub fn interfaces_json(sensors: &SensorSource) -> Value {
    let interfaces: Vec<Value> = enumerate_interfaces()
        .iter()
        .map(|n| {
            json!({
                "name": n.name, "rxBytes": n.rx_bytes, "txBytes": n.tx_bytes,
                "up": n.up, "wireless": n.wireless, "virtual": n.is_virtual,
            })
        })
        .collect();
    json!({
        "interfaces": interfaces,
        "selected": sensors.net_selection(),
        "active": sensors.latest().net_interface,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    fn temp_dir() -> PathBuf {
        // A pid + nanos + monotonic counter makes the dir unique even when tests
        // run in parallel in the same nanosecond (common with the default
        // multithreaded harness), so parallel tests never share/delete each
        // other's fixture dir. This removes an intermittent shared-state race.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "edgeline-sensors-test-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn put(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, format!("{text}\n")).unwrap();
    }

    #[test]
    fn parses_nvidia_smi_with_two_cards() {
        let two_cards =
            "GPU-8d7cf61a-e7d0-8304-b0b0-7d2bf89ae4f5, NVIDIA GeForce RTX 5090, 47, 0, 1369, 32607, 30.46\n\
             GPU-11111111-2222-3333-4444-555555555555, NVIDIA GeForce RTX 3060, 51, 88, 4096, 12288, 140.00\n";
        let nv = parse_nvidia_smi(two_cards);
        assert_eq!(nv.len(), 2);
        assert_eq!(nv[0].id, "nvidia:GPU-8d7cf61a-e7d0-8304-b0b0-7d2bf89ae4f5");
        assert_eq!(nv[0].name, "NVIDIA GeForce RTX 5090");
        assert_eq!(nv[0].source, "nvidia-smi");
        assert!(near(nv[0].temp_c, 47.0, 1e-6));
        assert!(near(nv[0].util_pct, 0.0, 1e-6));
        assert!(near(nv[0].mem_total_gib, 32607.0 / 1024.0, 1e-6));
        assert!(near(nv[0].power_w, 30.46, 1e-6));
        assert!(near(nv[1].util_pct, 88.0, 1e-6));
        assert_ne!(nv[0].id, nv[1].id);
    }

    #[test]
    fn nvidia_na_and_comma_names_and_junk() {
        let na = parse_nvidia_smi("GPU-abc, NVIDIA T400, [N/A], 3, 100, 2048, [N/A]\n");
        assert_eq!(na.len(), 1);
        assert!(na[0].temp_c < 0.0);
        assert!(na[0].power_w < 0.0);
        assert!(near(na[0].util_pct, 3.0, 1e-6));

        let comma = parse_nvidia_smi("GPU-xyz, Quadro RTX 8000, Passive, 60, 10, 512, 49152, 70.5\n");
        assert_eq!(comma.len(), 1);
        assert_eq!(comma[0].name, "Quadro RTX 8000, Passive");
        assert!(near(comma[0].power_w, 70.5, 1e-6));
        assert!(near(comma[0].temp_c, 60.0, 1e-6));

        assert!(parse_nvidia_smi("").is_empty());
        assert!(parse_nvidia_smi("bash: nvidia-smi: command not found\n").is_empty());
    }

    #[test]
    fn amd_sample_maps_to_gpu_info() {
        let amd = AmdGpuSample {
            card: "card2".to_string(),
            bus_address: "0000:72:00.0".to_string(),
            device_id: "13c0".to_string(),
            busy_pct: 4.0,
            vram_used_gib: 0.4,
            vram_total_gib: 2.0,
            temp_c: 41.0,
            power_w: 0.009,
        };
        let g = from_amd_sample(&amd, "AMD Radeon Graphics");
        assert_eq!(g.id, "amdgpu:0000:72:00.0");
        assert_eq!(g.source, "amdgpu");
        assert!(near(g.util_pct, 4.0, 1e-6));
        assert!(near(g.mem_total_gib, 2.0, 1e-6));

        let no_addr = AmdGpuSample {
            card: "card0".to_string(),
            ..AmdGpuSample {
                card: String::new(),
                bus_address: String::new(),
                device_id: String::new(),
                busy_pct: 1.0,
                vram_used_gib: -1.0,
                vram_total_gib: -1.0,
                temp_c: -1.0,
                power_w: -1.0,
            }
        };
        assert_eq!(from_amd_sample(&no_addr, "x").id, "amdgpu:card0");
    }

    #[test]
    fn gpu_selection_matches_cpp_cases() {
        let nv = parse_nvidia_smi(
            "GPU-8d7cf61a-e7d0-8304-b0b0-7d2bf89ae4f5, NVIDIA GeForce RTX 5090, 47, 0, 1369, 32607, 30.46\n",
        );
        assert_eq!(nv.len(), 1);
        let ig = from_amd_sample(
            &AmdGpuSample {
                card: "card2".to_string(),
                bus_address: "0000:72:00.0".to_string(),
                device_id: "13c0".to_string(),
                busy_pct: 4.0,
                vram_used_gib: 0.4,
                vram_total_gib: 2.0,
                temp_c: 41.0,
                power_w: 0.009,
            },
            "AMD Radeon Graphics",
        );
        let both = vec![nv[0].clone(), ig.clone()];

        // Automatic takes the card with the most VRAM (the discrete one).
        let auto = select_gpus(&both, &[]);
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].id, nv[0].id);

        let just_amd = select_gpus(&both, &[ig.id.clone()]);
        assert_eq!(just_amd.len(), 1);
        assert_eq!(just_amd[0].id, ig.id);

        let pair = select_gpus(&both, &[ig.id.clone(), nv[0].id.clone()]);
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].id, nv[0].id); // detection order wins
        assert_eq!(pair[1].id, ig.id);

        // A repeated id is one tile, not two.
        assert_eq!(select_gpus(&both, &[ig.id.clone(), ig.id.clone()]).len(), 1);

        // A stale id drops the card and keeps the rest.
        let stale = select_gpus(&both, &["nvidia:gone".to_string(), ig.id.clone()]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, ig.id);

        // If every saved id is stale, fall back to automatic.
        let all_stale = select_gpus(&both, &["nvidia:gone".to_string()]);
        assert_eq!(all_stale.len(), 1);
        assert_eq!(all_stale[0].id, nv[0].id);

        assert!(select_gpus(&[], &[]).is_empty());
        assert!(select_gpus(&[], &[ig.id.clone()]).is_empty());
    }

    #[test]
    fn reads_amd_gpus_from_fixture_tree() {
        let root = temp_dir();
        put(&root.join("card0/device/vendor"), "0x1002");
        put(&root.join("card0/device/device"), "0x13c0");
        put(&root.join("card0/device/gpu_busy_percent"), "7");
        put(&root.join("card0/device/mem_info_vram_used"), "55566336");
        put(&root.join("card0/device/mem_info_vram_total"), "2147483648");
        put(&root.join("card0/device/hwmon/hwmon7/name"), "amdgpu");
        put(&root.join("card0/device/hwmon/hwmon7/temp1_input"), "40000");
        put(&root.join("card0/device/hwmon/hwmon7/power1_input"), "11000000");

        put(&root.join("card1/device/vendor"), "0x1002");
        put(&root.join("card1/device/device"), "0x7590");
        put(&root.join("card1/device/gpu_busy_percent"), "63");
        put(&root.join("card1/device/mem_info_vram_used"), "4294967296");
        put(&root.join("card1/device/mem_info_vram_total"), "17179869184");
        put(&root.join("card1/device/hwmon/hwmon3/name"), "amdgpu");
        put(&root.join("card1/device/hwmon/hwmon3/temp1_input"), "61000");
        put(&root.join("card1/device/hwmon/hwmon3/power1_average"), "148000000");

        // NVIDIA is ignored no matter what files it has.
        put(&root.join("card2/device/vendor"), "0x10de");
        put(&root.join("card2/device/gpu_busy_percent"), "99");
        put(&root.join("card2/device/mem_info_vram_total"), "34359738368");

        // AMD vendor but nothing the amdgpu driver writes is skipped.
        put(&root.join("card3/device/vendor"), "0x1002");
        put(&root.join("card3/device/device"), "0x6779");

        // Connector entries live beside the cards and must not be mistaken for one.
        put(&root.join("card1-DP-1/status"), "connected");

        let cards = read_amd_gpus(&root);
        assert_eq!(cards.len(), 2);

        let igpu = cards.iter().find(|c| c.card == "card0").unwrap();
        let dgpu = cards.iter().find(|c| c.card == "card1").unwrap();

        assert_eq!(igpu.device_id, "13c0");
        assert!(near(igpu.busy_pct, 7.0, 1e-6));
        assert!(near(igpu.vram_total_gib, 2.0, 1e-6));
        assert!(near(igpu.temp_c, 40.0, 1e-6));
        assert!(near(igpu.power_w, 11.0, 1e-6)); // power1_input, microwatts
        assert!(igpu.bus_address.is_empty()); // fixture has no PCI address

        assert_eq!(dgpu.device_id, "7590");
        assert!(near(dgpu.busy_pct, 63.0, 1e-6));
        assert!(near(dgpu.vram_used_gib, 4.0, 1e-6));
        assert!(near(dgpu.vram_total_gib, 16.0, 1e-6));
        assert!(near(dgpu.temp_c, 61.0, 1e-6));
        assert!(near(dgpu.power_w, 148.0, 1e-6)); // power1_average preferred

        let pick = pick_amd_gpu(&cards);
        assert_eq!(pick.unwrap().card, "card1");

        // Missing sensors degrade to -1, not to zero and not to a skipped card.
        put(&root.join("card4/device/vendor"), "0x1002");
        put(&root.join("card4/device/device"), "0x744c");
        put(&root.join("card4/device/gpu_busy_percent"), "12");
        let more = read_amd_gpus(&root);
        assert_eq!(more.len(), 3);
        let card4 = more.iter().find(|c| c.card == "card4").unwrap();
        assert!(near(card4.busy_pct, 12.0, 1e-6));
        assert!(card4.vram_total_gib < 0.0);
        assert!(card4.temp_c < 0.0);
        assert!(card4.power_w < 0.0);

        assert!(read_amd_gpus(&root.join("does-not-exist")).is_empty());
        assert!(pick_amd_gpu(&[]).is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cpu_load_delta_needs_a_baseline() {
        let dir = temp_dir();
        let stat = dir.join("stat");
        put(&stat, "cpu  100 0 100 400 100 0 300");
        let mut state = LoadState::default();
        assert_eq!(read_cpu_load(&stat, &mut state), -1.0); // first call: no delta

        put(&stat, "cpu  150 0 150 500 100 0 300");
        let load = read_cpu_load(&stat, &mut state);
        // total 1000 -> 1200 (dt 200), idle 500 -> 600 (di 100), load 50%.
        assert!(near(load, 50.0, 1e-6));

        assert_eq!(read_cpu_load(&dir.join("missing"), &mut state), -1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_is_available_based() {
        let dir = temp_dir();
        let mem = dir.join("meminfo");
        put(&mem, "MemTotal:       16777216 kB\nMemFree:         1000000 kB\nMemAvailable:    8388608 kB");
        let (used, total, pct) = read_memory(&mem);
        assert!(near(total, 16.0, 1e-6));
        assert!(near(used, 8.0, 1e-6));
        assert!(near(pct, 50.0, 1e-6));

        put(&mem, "MemTotal:       0 kB");
        let (used, total, pct) = read_memory(&mem);
        assert_eq!(used, 0.0);
        assert_eq!(total, 0.0);
        assert_eq!(pct, -1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uptime_and_topology_parse() {
        let dir = temp_dir();
        put(&dir.join("uptime"), "12345.67 45678.90");
        assert_eq!(read_uptime(&dir.join("uptime")), 12345);
        assert_eq!(read_uptime(&dir.join("missing")), -1);

        put(
            &dir.join("cpuinfo"),
            "processor\t: 0\nphysical id\t: 0\ncore id\t\t: 0\n\
             processor\t: 1\nphysical id\t: 0\ncore id\t\t: 1\n\
             processor\t: 2\nphysical id\t: 0\ncore id\t\t: 0\n",
        );
        let (cores, threads) = read_cpu_topology(&dir.join("cpuinfo"));
        assert_eq!(threads, 3);
        assert_eq!(cores, 2); // physical 0 has core ids 0 and 1
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn network_picks_physical_up_interface_and_deltas() {
        let dir = temp_dir();
        let netdev = dir.join("net_dev");
        let sys = dir.join("net");
        put(
            &netdev,
            "Inter-|   Receive                                                |  Transmit\n\
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
             lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n\
             eth0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        );
        // eth0 is physical and up; lo is virtual. A "wireless" dir marks eth0 wireless.
        put(&sys.join("eth0/operstate"), "up");
        put(&sys.join("eth0/carrier"), "1");
        std::fs::create_dir_all(sys.join("eth0/wireless")).unwrap();
        put(&sys.join("lo/operstate"), "up");

        let mut state = NetworkState::default();
        // First call registers the baseline, no delta.
        let (name, rx, tx) = read_network(&netdev, &sys, 1000, &mut state);
        assert_eq!(name, "eth0");
        assert_eq!(rx, -1.0);
        assert_eq!(tx, -1.0);

        // Let eth0 move 1 MiB rx / 2 MiB tx over 0.5 s.
        put(
            &netdev,
            "Inter-|   Receive                                                |  Transmit\n\
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
             lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n\
             eth0: 1048576 0 0 0 0 0 0 0 2097152 0 0 0 0 0 0 0\n",
        );
        let (name, rx, tx) = read_network(&netdev, &sys, 1500, &mut state);
        assert_eq!(name, "eth0");
        assert!(near(rx, 2.0, 1e-6));
        assert!(near(tx, 4.0, 1e-6));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn network_pinned_selection_and_virtual_skip() {
        let dir = temp_dir();
        let netdev = dir.join("net_dev");
        let sys = dir.join("net");
        put(
            &netdev,
            "face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n\
             lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n\
             docker0: 200 0 0 0 0 0 0 0 200 0 0 0 0 0 0 0\n\
             wg0: 300 0 0 0 0 0 0 0 300 0 0 0 0 0 0 0\n\
             eth0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        );
        put(&sys.join("lo/operstate"), "up");
        put(&sys.join("docker0/operstate"), "up");
        put(&sys.join("wg0/operstate"), "up");
        put(&sys.join("eth0/operstate"), "up");

        // Automatic skips the virtual interfaces and picks eth0 despite it
        // having fewer lifetime bytes.
        let mut state = NetworkState::default();
        let (name, ..) = read_network(&netdev, &sys, 1000, &mut state);
        assert_eq!(name, "eth0");

        // Pinning to a virtual interface (a WireGuard tunnel) wins.
        let mut state = NetworkState {
            selection: "wg0".to_string(),
            ..NetworkState::default()
        };
        let (name, ..) = read_network(&netdev, &sys, 2000, &mut state);
        assert_eq!(name, "wg0");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn disk_picks_whole_device_and_deltas() {
        let dir = temp_dir();
        let stats = dir.join("diskstats");
        put(
            &stats,
            "  8       0 sda 0 0 1000 0 0 0 2000 0 0 0 0 0\n\
             8       1 sda1 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
           259       0 nvme0n1 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        );
        let mut state = DiskState::default();
        let (name, rd, wr) = read_disk(&stats, 1000, &mut state);
        assert_eq!(name, "sda");
        assert_eq!(rd, -1.0);
        assert_eq!(wr, -1.0);

        // sda moves 1 MiB read / 2 MiB written over 0.5 s from the baseline
        // captured above; the whole-disk rule skips the sda1 partition.
        put(
            &stats,
            "  8       0 sda 0 0 3048 0 0 0 6096 0 0 0 0 0\n\
             8       1 sda1 0 0 0 0 0 0 0 0 0 0 0 0 0\n\
           259       0 nvme0n1 0 0 1024 0 0 0 2048 0 0 0 0 0\n",
        );
        let (name, rd, wr) = read_disk(&stats, 1500, &mut state);
        assert_eq!(name, "sda");
        assert!(near(rd, 2.0, 1e-6));
        assert!(near(wr, 4.0, 1e-6));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fan_reads_fastest_spinning_chip() {
        let dir = temp_dir();
        put(&dir.join("hwmon0/name"), "k10temp");
        put(&dir.join("hwmon0/fan1_input"), "1200");
        put(&dir.join("hwmon1/name"), "nct6798");
        put(&dir.join("hwmon1/fan1_input"), "800");
        put(&dir.join("hwmon1/fan2_input"), "not-a-number");

        let (rpm, chip) = read_fan(&dir);
        assert!(near(rpm, 1200.0, 1e-6));
        assert_eq!(chip, "k10temp");

        assert_eq!(read_fan(&dir.join("missing")), (-1.0, String::new()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn package_power_derives_watts_from_energy() {
        let dir = temp_dir();
        let rapl = dir.join("intel-rapl:0");
        let energy = rapl.join("energy_uj");
        put(&energy, "1000000");

        let mut state = PowerState::default();
        // First call resolves the path and registers the baseline, no watts.
        assert_eq!(read_package_power(&dir, 1000, &mut state), -1.0);
        assert_eq!(state.path, energy.to_string_lossy());

        put(&energy, "1500000"); // 0.5 J gained over 0.5 s
        assert!(near(read_package_power(&dir, 1500, &mut state), 1.0, 1e-6));

        // A counter wrap (decrease) is not negative power.
        put(&energy, "100000");
        assert_eq!(read_package_power(&dir, 2000, &mut state), -1.0);

        // No readable counter means no source.
        let empty = temp_dir();
        let mut s2 = PowerState::default();
        assert_eq!(read_package_power(&empty, 1000, &mut s2), -1.0);
        assert_eq!(read_package_power(&empty, 1500, &mut s2), -1.0);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&empty).unwrap();
    }

    #[test]
    fn failed_units_parse_and_disk_usage_is_consistent() {
        assert_eq!(
            parse_failed_units("foo.service loaded failed failed Foo\nbar.service loaded failed failed Bar\n"),
            vec!["foo.service".to_string(), "bar.service".to_string()]
        );
        assert!(parse_failed_units("").is_empty());

        // Pure, deterministic: the percentage is a straight function of the
        // block counts, so this must be exact and never flaky.
        let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
        vfs.f_blocks = 1000;
        vfs.f_bavail = 750;
        assert_eq!(disk_usage_from(vfs), 25.0);
        vfs.f_bavail = 0;
        assert_eq!(disk_usage_from(vfs), 100.0);
        vfs.f_bavail = 1000;
        assert_eq!(disk_usage_from(vfs), 0.0);

        // A live smoke check: read_disk_usage must return a sane percentage.
        // We deliberately do NOT compare it to a second live statvfs, because
        // two independent syscalls on a busy filesystem can legitimately differ
        // (f_bavail is not stable), which made this test flaky. The pure
        // disk_usage_from assertions above already prove the math.
        let dir = temp_dir();
        let got = read_disk_usage(&dir);
        assert!(got.is_finite() && (0.0..=100.0).contains(&got), "disk usage out of range: {got}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn snapshot_json_matches_cpp_contract() {
        let mut snap = Snapshot::default();
        snap.cpu_load_pct = 42.0;
        snap.cpu_temp_c = 61.0;
        snap.net_interface = "eth0".to_string();
        snap.net_rx_mbs = 2.0;
        snap.disk_device = "sda".to_string();
        snap.gpus = vec![GpuInfo {
            id: "nvidia:abc".to_string(),
            name: "NVIDIA GeForce RTX 5090".to_string(),
            source: "nvidia-smi".to_string(),
            util_pct: 7.0,
            temp_c: 55.0,
            mem_used_gib: 1.0,
            mem_total_gib: 32.0,
            power_w: 30.0,
        }];
        snap.gpu_ids_shown = vec!["nvidia:abc".to_string()];
        snap.gpu_ok = true;
        snap.gpu_name = "NVIDIA GeForce RTX 5090".to_string();
        snap.gpu_source = "nvidia-smi".to_string();

        let v = snapshot_json(&snap);
        assert_eq!(v["netInterface"], "eth0");
        assert_eq!(v["cpuLoadPct"], 42.0);
        assert_eq!(v["gpus"].as_array().unwrap().len(), 1);
        assert_eq!(v["gpus"][0]["id"], "nvidia:abc");
        assert_eq!(v["gpuIdsShown"].as_array().unwrap().len(), 1);
        assert_eq!(v["sessionBusOk"], false);
        assert_eq!(v["notifications"].as_array().unwrap().len(), 0);
    }
}
