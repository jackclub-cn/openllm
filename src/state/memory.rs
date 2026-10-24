use super::*;

/// How long a sampled memory reading is reused before it is refreshed.
pub(super) const MEMORY_SAMPLE_TTL: Duration = Duration::from_millis(500);

/// Process-memory pressure guard.
///
/// Under sustained load the process's own footprint can grow past the point
/// where new work is safe (large bodies, translations, SQLite). This guard
/// compares the live resident-set size against an operator-declared ceiling and
/// sheds new proxied requests once usage crosses a fraction of it, so the
/// process keeps headroom to finish what is already in flight instead of being
/// killed by the OOM killer. It uses the built-in 768 MiB default unless a
/// tighter cgroup limit is discovered or `OPENLLM_MEMORY_LIMIT_MIB=0` is set.
pub struct MemoryPressure {
    pub(super) limit_bytes: u64,
    pub(super) shed_ratio: f64,
    /// Cached `(sampled_at, usage)` so the hot path does not read `/proc` per
    /// request.
    pub(super) sample: std::sync::Mutex<Option<(Instant, Option<u64>)>>,
    /// Injectable usage reader; tests swap in a fixed value.
    pub(super) usage: Box<dyn Fn() -> Option<u64> + Send + Sync>,
}

impl MemoryPressure {
    pub fn new(limit_bytes: u64, shed_ratio: f64) -> Self {
        Self {
            limit_bytes,
            shed_ratio,
            sample: std::sync::Mutex::new(None),
            usage: Box::new(read_process_memory_bytes),
        }
    }

    /// A guard with a fixed usage reading, for deterministic tests.
    #[cfg(test)]
    pub fn with_usage(limit_bytes: u64, shed_ratio: f64, used_bytes: u64) -> Self {
        Self {
            limit_bytes,
            shed_ratio,
            sample: std::sync::Mutex::new(None),
            usage: Box::new(move || Some(used_bytes)),
        }
    }

    pub fn limit_bytes(&self) -> u64 {
        self.limit_bytes
    }

    pub fn shed_ratio(&self) -> f64 {
        self.shed_ratio
    }

    /// Usage at (or above) which new requests are shed.
    pub fn threshold_bytes(&self) -> u64 {
        (self.limit_bytes as f64 * self.shed_ratio).round() as u64
    }

    /// Whether `used_bytes` is at or past the shed threshold.
    pub fn is_shedding_at(&self, used_bytes: u64) -> bool {
        used_bytes >= self.threshold_bytes()
    }

    /// Cached current usage; `None` when the platform cannot report it.
    pub fn used_bytes(&self) -> Option<u64> {
        let mut guard = self
            .sample
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some((at, value)) = *guard
            && at.elapsed() < MEMORY_SAMPLE_TTL
        {
            return value;
        }
        let value = (self.usage)();
        *guard = Some((Instant::now(), value));
        value
    }

    /// Whether new proxied requests should be shed right now.
    pub fn is_shedding(&self) -> bool {
        self.used_bytes()
            .is_some_and(|used| self.is_shedding_at(used))
    }
}

/// Reads this process's resident set size where the OS exposes it cheaply.
///
/// Linux and Windows are supported; every other platform returns `None`, which
/// keeps the guard inert rather than guessing. The ceiling is meant to be the
/// container limit, but resident memory (not OS page cache) is compared against
/// it, because that is the part which actually cannot be reclaimed under
/// pressure.
#[cfg(target_os = "linux")]
pub(super) fn read_process_memory_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Windows exposes the process working set through `GetProcessMemoryInfo`.
#[cfg(target_os = "windows")]
pub(super) fn read_process_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let mut counters = std::mem::MaybeUninit::<ProcessMemoryCounters>::zeroed();
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // SAFETY: `counters` is a correctly sized, zeroed buffer and `cb` matches
    // its size, which is exactly what `GetProcessMemoryInfo` documents; the
    // call only writes into that buffer.
    unsafe {
        (*counters.as_mut_ptr()).cb = size;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), size) == 0 {
            return None;
        }
        Some((*counters.as_ptr()).working_set_size as u64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(super) fn read_process_memory_bytes() -> Option<u64> {
    None
}

/// Samples the process's current resident/working-set size in bytes.
///
/// Exposed so the runtime-status endpoint can report memory even when the
/// pressure guard is disabled; `None` when the platform cannot report it.
pub(crate) fn sample_process_memory_bytes() -> Option<u64> {
    read_process_memory_bytes()
}

/// Discovers the container memory limit where the OS exposes one.
///
/// Reads the cgroup v2 unified hierarchy first, then the cgroup v1 memory
/// controller. `None` means "not in a constrained container" (or a non-Linux
/// host), which leaves the built-in default as the automatic ceiling.
#[cfg(target_os = "linux")]
pub(super) fn cgroup_memory_max_bytes() -> Option<u64> {
    let v2 = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok();
    if let Some(bytes) = parse_cgroup_memory_max(v2.as_deref()) {
        return Some(bytes);
    }
    let v1 = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes").ok();
    parse_cgroup_memory_max(v1.as_deref())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_memory_max_bytes() -> Option<u64> {
    None
}
