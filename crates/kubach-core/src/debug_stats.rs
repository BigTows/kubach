//! Metrics of Kubach itself for the debug overlay (#291): frame rate and
//! frame time of the window, CPU, memory and threads of the process.
//!
//! Compiled only into debug builds (`cfg(debug_assertions)`); release builds
//! have neither the overlay nor this module. Process figures come from the
//! OS directly (`proc_pidinfo` on macOS, `/proc/self` on Linux, the process
//! status API on Windows), so no dependency is added for them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back frame times are averaged.
pub const FRAME_SPAN: Duration = Duration::from_secs(5);

/// Frames per second are counted over this span.
const FPS_SPAN: Duration = Duration::from_secs(1);

/// Frames drawn over the last [`FRAME_SPAN`]: when each finished and how long
/// it took.
#[derive(Debug)]
pub struct FrameWindow {
    span: Duration,
    frames: VecDeque<(Instant, Duration)>,
}

/// Frame rate and frame times at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSummary {
    /// Frames drawn over the last second; 0 while the window is idle.
    pub fps: usize,
    /// The most recent frame, if one was drawn within the span.
    pub last: Option<Duration>,
    /// Average and longest frame within the span.
    pub avg: Option<Duration>,
    pub max: Option<Duration>,
}

impl FrameSummary {
    /// No frame was drawn over the last second: nothing changed on screen.
    pub fn is_idle(&self) -> bool {
        self.fps == 0
    }
}

impl Default for FrameWindow {
    fn default() -> Self {
        Self::new(FRAME_SPAN)
    }
}

impl FrameWindow {
    pub fn new(span: Duration) -> Self {
        Self {
            span: span.max(FPS_SPAN),
            frames: VecDeque::new(),
        }
    }

    /// A frame finished at `at` after `duration` of work.
    pub fn record(&mut self, at: Instant, duration: Duration) {
        self.frames.push_back((at, duration));
        self.prune(at);
    }

    /// Drops frames older than the span before `now`.
    fn prune(&mut self, now: Instant) {
        while let Some(&(at, _)) = self.frames.front() {
            if now.saturating_duration_since(at) <= self.span {
                break;
            }
            self.frames.pop_front();
        }
    }

    pub fn summary(&mut self, now: Instant) -> FrameSummary {
        self.prune(now);
        let fps = self
            .frames
            .iter()
            .rev()
            .take_while(|(at, _)| now.saturating_duration_since(*at) < FPS_SPAN)
            .count();
        let count = self.frames.len() as u32;
        let total: Duration = self.frames.iter().map(|(_, duration)| *duration).sum();
        FrameSummary {
            fps,
            last: self.frames.back().map(|(_, duration)| *duration),
            avg: (count > 0).then(|| total / count),
            max: self.frames.iter().map(|(_, duration)| *duration).max(),
        }
    }
}

/// CPU usage from successive readings of the process CPU time.
#[derive(Debug, Default)]
pub struct CpuMeter {
    last: Option<(Instant, Duration)>,
}

impl CpuMeter {
    /// Records the total CPU time `cpu` of the process at `at`; returns the
    /// usage since the previous reading in percent of one core (above 100
    /// when several cores are busy, like `top`). `None` for the first
    /// reading, or when no time passed or the counter went back.
    pub fn sample(&mut self, at: Instant, cpu: Duration) -> Option<f32> {
        let previous = self.last.replace((at, cpu));
        let (then, cpu_then) = previous?;
        let wall = at.checked_duration_since(then)?;
        let used = cpu.checked_sub(cpu_then)?;
        if wall.is_zero() {
            return None;
        }
        Some((used.as_secs_f64() / wall.as_secs_f64() * 100.) as f32)
    }
}

/// Resources the process uses at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessSample {
    /// User plus system CPU time since the start.
    pub cpu_time: Duration,
    /// Resident memory (working set on Windows), in bytes.
    pub rss: u64,
    pub threads: usize,
}

/// Reads CPU time, resident memory and thread count of this process; `None`
/// where the OS refused or the platform is not supported. Makes system
/// calls (and reads `/proc` on Linux): keep it off the UI thread.
pub fn sample_process() -> Option<ProcessSample> {
    platform::sample()
}

/// Total user and system CPU time of the process (`getrusage`).
#[cfg(unix)]
fn rusage_cpu_time() -> Option<Duration> {
    // SAFETY: `getrusage` fills the zeroed struct it is given.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        usage
    };
    let time = |tv: libc::timeval| {
        Duration::from_secs(tv.tv_sec.max(0) as u64)
            + Duration::from_micros(tv.tv_usec.max(0) as u64)
    };
    Some(time(usage.ru_utime) + time(usage.ru_stime))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{ProcessSample, rusage_cpu_time};

    pub fn sample() -> Option<ProcessSample> {
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: `proc_pidinfo` writes at most `size` bytes into the zeroed
        // struct and returns how many it wrote.
        let info = unsafe {
            let mut info: libc::proc_taskinfo = std::mem::zeroed();
            let written = libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                (&mut info as *mut libc::proc_taskinfo).cast(),
                size,
            );
            if written != size {
                return None;
            }
            info
        };
        Some(ProcessSample {
            // `pti_total_user` is in Mach ticks on Apple silicon; rusage is
            // in real time everywhere.
            cpu_time: rusage_cpu_time()?,
            rss: info.pti_resident_size,
            threads: info.pti_threadnum.max(0) as usize,
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{ProcessSample, rusage_cpu_time};

    pub fn sample() -> Option<ProcessSample> {
        // `statm`: size resident shared … in pages.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: `sysconf` only reads a configuration value.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        Some(ProcessSample {
            cpu_time: rusage_cpu_time()?,
            rss: resident * u64::try_from(page).ok()?,
            threads: super::status_threads(&status)?,
        })
    }
}

/// The `Threads:` line of `/proc/<pid>/status`.
#[cfg(any(target_os = "linux", test))]
fn status_threads(status: &str) -> Option<usize> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))?
        .trim()
        .parse()
        .ok()
}

#[cfg(windows)]
mod platform {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessTimes,
    };

    use super::ProcessSample;

    pub fn sample() -> Option<ProcessSample> {
        Some(ProcessSample {
            cpu_time: cpu_time()?,
            rss: working_set()?,
            threads: threads()?,
        })
    }

    fn cpu_time() -> Option<Duration> {
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [creation, exit, kernel, user] = &mut times;
        // SAFETY: the pseudo handle of the current process needs no closing;
        // the call fills the four FILETIMEs.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) };
        if ok == 0 {
            return None;
        }
        // FILETIME durations count 100 ns intervals.
        let ticks = |time: &FILETIME| {
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
        };
        Some(Duration::from_nanos((ticks(kernel) + ticks(user)) * 100))
    }

    fn working_set() -> Option<u64> {
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size,
            ..Default::default()
        };
        // SAFETY: `counters` is as large as `cb` says.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        (ok != 0).then_some(counters.WorkingSetSize as u64)
    }

    /// Threads of this process in a snapshot of all threads of the system.
    fn threads() -> Option<usize> {
        // SAFETY: the snapshot handle is checked and closed below; the
        // entries are sized through `dwSize`.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let pid = GetCurrentProcessId();
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut count = 0;
            let mut more = Thread32First(snapshot, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    count += 1;
                }
                more = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            Some(count)
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    pub fn sample() -> Option<super::ProcessSample> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{CpuMeter, FrameWindow, sample_process, status_threads};

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn no_frames_is_idle() {
        let mut frames = FrameWindow::default();
        let summary = frames.summary(Instant::now());
        assert!(summary.is_idle());
        assert_eq!(summary.last, None);
        assert_eq!(summary.avg, None);
    }

    #[test]
    fn counts_frames_of_the_last_second() {
        let start = Instant::now();
        let mut frames = FrameWindow::default();
        // 60 frames over one second, 4 ms each, then the last one took 10 ms.
        for ix in 0..60 {
            frames.record(start + Duration::from_millis(ix * 16), 4 * MS);
        }
        frames.record(start + 960 * MS, 10 * MS);
        let summary = frames.summary(start + 970 * MS);
        assert_eq!(summary.fps, 61);
        assert_eq!(summary.last, Some(10 * MS));
        assert_eq!(summary.max, Some(10 * MS));
        assert_eq!(summary.avg, Some((60 * 4 * MS + 10 * MS) / 61));
    }

    #[test]
    fn old_frames_leave_the_rate_then_the_window() {
        let start = Instant::now();
        let mut frames = FrameWindow::new(Duration::from_secs(5));
        frames.record(start, 30 * MS);
        frames.record(start + 100 * MS, 2 * MS);

        // Two seconds later: idle, but the times are still in the window.
        let summary = frames.summary(start + Duration::from_secs(2));
        assert!(summary.is_idle());
        assert_eq!(summary.max, Some(30 * MS));
        assert_eq!(summary.last, Some(2 * MS));

        // Past the window nothing is left.
        let summary = frames.summary(start + Duration::from_secs(6));
        assert_eq!(summary.max, None);
        assert_eq!(summary.avg, None);
    }

    #[test]
    fn cpu_usage_from_deltas() {
        let start = Instant::now();
        let mut meter = CpuMeter::default();
        assert_eq!(meter.sample(start, Duration::from_secs(10)), None);
        // 250 ms of CPU over one second: 25 %.
        let usage = meter
            .sample(
                start + Duration::from_secs(1),
                Duration::from_millis(10_250),
            )
            .unwrap();
        assert!((usage - 25.).abs() < 0.01, "{usage}");
        // Two busy cores for half a second: 200 %.
        let usage = meter
            .sample(start + 1500 * MS, Duration::from_millis(11_250))
            .unwrap();
        assert!((usage - 200.).abs() < 0.01, "{usage}");
    }

    #[test]
    fn cpu_usage_skips_bad_readings() {
        let start = Instant::now();
        let mut meter = CpuMeter::default();
        meter.sample(start, Duration::from_secs(5));
        // No time passed.
        assert_eq!(meter.sample(start, Duration::from_secs(6)), None);
        // The counter went back.
        assert_eq!(
            meter.sample(start + Duration::from_secs(1), Duration::from_secs(1)),
            None
        );
        // The next reading counts from the last one again.
        let usage = meter
            .sample(start + Duration::from_secs(2), 1500 * MS)
            .unwrap();
        assert!((usage - 50.).abs() < 0.01, "{usage}");
    }

    #[test]
    fn threads_from_proc_status() {
        let status = "Name:\tkubach\nState:\tS (sleeping)\nThreads:\t17\nVmRSS:\t1 kB\n";
        assert_eq!(status_threads(status), Some(17));
        assert_eq!(status_threads("Name:\tkubach\n"), None);
    }

    #[test]
    fn samples_this_process() {
        let sample = sample_process().expect("process metrics");
        assert!(sample.rss > 0);
        assert!(sample.threads >= 1);
        // Burn a little CPU so the time can't be zero.
        let mut x = 0u64;
        for ix in 0..2_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(ix);
        }
        std::hint::black_box(x);
        let later = sample_process().unwrap();
        assert!(later.cpu_time >= sample.cpu_time);
    }
}
