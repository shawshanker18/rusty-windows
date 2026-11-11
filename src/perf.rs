//! Live CPU / RAM / disk / GPU usage and CPU / GPU temperature for Quick Settings, from the same
//! sources Task Manager uses (no admin needed): GetSystemTimes, GlobalMemoryStatusEx, PDH counters
//! (PhysicalDisk, GPU Engine, Thermal Zone) and NVIDIA's driver library (nvml.dll) for GPU temperature.
//! One sampler thread runs only while Quick Settings is open.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Performance::*;

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub cpu: Option<f32>,              // %
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub disk_active: Option<f32>,      // % active time, all disks
    pub disk_free_gb: Option<f32>,     // C:
    pub gpu: Option<f32>,              // % (3D engines, like Task Manager)
    pub cpu_temp: Option<f32>,         // °C, hottest ACPI thermal zone
    pub gpu_temp: Option<f32>,         // °C, NVIDIA
}

impl Stats {
    pub fn ram_pct(&self) -> f32 {
        if self.ram_total_gb > 0.0 { 100.0 * self.ram_used_gb / self.ram_total_gb } else { 0.0 }
    }
}

pub static STATS: Mutex<Option<Stats>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn latest() -> Stats {
    STATS.lock().unwrap().unwrap_or_default()
}

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

fn ram() -> (f32, f32) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        if GlobalMemoryStatusEx(&mut m).is_err() {
            return (0.0, 0.0);
        }
    }
    let gb = |b: u64| b as f32 / 1024.0 / 1024.0 / 1024.0;
    (gb(m.ullTotalPhys - m.ullAvailPhys), gb(m.ullTotalPhys))
}

fn disk_space() -> (Option<f32>, Option<f32>) {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let (mut free, mut total) = (0u64, 0u64);
    unsafe {
        if GetDiskFreeSpaceExW(w!("C:\\"), None, Some(&mut total), Some(&mut free)).is_err() {
            return (None, None);
        }
    }
    let gb = |b: u64| b as f32 / 1024.0 / 1024.0 / 1024.0;
    (Some(gb(free)), Some(gb(total)))
}

/// NVIDIA GPU temperature through the driver's NVML library (absent on non-NVIDIA PCs).
struct Nvml {
    temp: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut u32) -> i32,
    shutdown: unsafe extern "system" fn() -> i32,
    dev: *mut core::ffi::c_void,
}

fn nvml() -> Option<Nvml> {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let lib = LoadLibraryW(w!("nvml.dll")).ok()?;
        let init: unsafe extern "system" fn() -> i32 = std::mem::transmute(GetProcAddress(lib, s!("nvmlInit_v2"))?);
        let by_index: unsafe extern "system" fn(u32, *mut *mut core::ffi::c_void) -> i32 =
            std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetHandleByIndex_v2"))?);
        let temp = std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetTemperature"))?);
        let shutdown: unsafe extern "system" fn() -> i32 = std::mem::transmute(GetProcAddress(lib, s!("nvmlShutdown"))?);
        if init() != 0 {
            return None;
        }
        let mut dev = std::ptr::null_mut();
        if by_index(0, &mut dev) != 0 {
            shutdown();
            return None;
        }
        Some(Nvml { temp, shutdown, dev })
    }
}

/// Sum (or max) over every instance of a wildcard PDH counter.
unsafe fn pdh_all(c: PDH_HCOUNTER, max: bool) -> Option<f32> {
    let (mut size, mut n) = (0u32, 0u32);
    let _ = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut n, None);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut n, Some(items)) != 0 {
        return None;
    }
    let vals = std::slice::from_raw_parts(items, n as usize).iter().map(|i| i.FmtValue.Anonymous.doubleValue as f32);
    Some(if max { vals.fold(f32::MIN, f32::max) } else { vals.sum() })
}

unsafe fn pdh_one(c: PDH_HCOUNTER) -> Option<f32> {
    let mut v = PDH_FMT_COUNTERVALUE::default();
    (PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) == 0).then(|| v.Anonymous.doubleValue as f32)
}

/// Start sampling once a second until `stop()`; each sample posts `notify()`.
pub fn start(notify: fn()) {
    if RUNNING.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(move || unsafe {
        let mut q = PDH_HQUERY::default();
        let ok = PdhOpenQueryW(PCWSTR::null(), 0, &mut q) == 0;
        let add = |path: PCWSTR| {
            let mut c = PDH_HCOUNTER::default();
            (ok && PdhAddEnglishCounterW(q, path, 0, &mut c) == 0).then_some(c)
        };
        let disk = add(w!("\\PhysicalDisk(_Total)\\% Idle Time"));
        let gpu = add(w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage"));
        let thermal = add(w!("\\Thermal Zone Information(*)\\Temperature"));
        let nv = nvml();
        let times = || {
            let (mut i, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
            let _ = windows::Win32::System::Threading::GetSystemTimes(Some(&mut i), Some(&mut k), Some(&mut u));
            (ft(i), ft(k) + ft(u)) // kernel time includes idle
        };
        let mut prev = times();
        if ok {
            PdhCollectQueryData(q);
        }
        while RUNNING.load(Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            if ok {
                PdhCollectQueryData(q);
            }
            let now = times();
            let (di, dt) = (now.0 - prev.0, now.1 - prev.1);
            prev = now;
            let (used, total) = ram();
            let (free, _) = disk_space();
            let st = Stats {
                cpu: (dt > 0).then(|| (100.0 * (dt - di) as f32 / dt as f32).clamp(0.0, 100.0)),
                ram_used_gb: used,
                ram_total_gb: total,
                disk_active: disk.and_then(|c| pdh_one(c)).map(|idle| (100.0 - idle).clamp(0.0, 100.0)),
                disk_free_gb: free,
                gpu: gpu.and_then(|c| pdh_all(c, false)).map(|v| v.clamp(0.0, 100.0)),
                // ACPI zones report Kelvin; ignore zones that read room temperature or less
                cpu_temp: thermal.and_then(|c| pdh_all(c, true)).map(|k| k - 273.15).filter(|t| *t > 25.0 && *t < 125.0),
                gpu_temp: nv.as_ref().and_then(|n| {
                    let mut t = 0u32;
                    ((n.temp)(n.dev, 0, &mut t) == 0).then_some(t as f32)
                }),
            };
            *STATS.lock().unwrap() = Some(st);
            notify();
        }
        if let Some(n) = nv {
            (n.shutdown)(); // NVML left initialised can hang the process at exit
        }
        if ok {
            PdhCloseQuery(q);
        }
    });
}

pub fn stop() {
    RUNNING.store(false, Relaxed);
}

/// Colour cue like Task Manager's graphs: normal, warm (>= 80), hot (>= 90).
pub fn heat(v: f32) -> u32 {
    if v >= 90.0 { 0xFF6B6B } else if v >= 80.0 { 0xFFB900 } else { 0x4CC2FF }
}

#[cfg(test)]
mod tests {
    #[test]
    fn heat_levels() {
        assert_eq!(super::heat(50.0), 0x4CC2FF);
        assert_eq!(super::heat(85.0), 0xFFB900);
        assert_eq!(super::heat(95.0), 0xFF6B6B);
    }
}
