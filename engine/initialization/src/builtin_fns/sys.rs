//! Environment, time/calendar, child process, PRNG, GC, and backtrace builtins (`616..=637`).

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use interpreter::resources::{IO_OK, io_error_code};
use interpreter::{BuiltinCtx, NativeValue, ProcessSpec, VmError};
use runtime::Number;

// ---------------------------------------------------------------------------
// 616..=622: Environment & platform introspection
// ---------------------------------------------------------------------------

/// `616` `__env_args() -> List`
pub fn env_args(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let items: Vec<NativeValue> = ctx
        .script_args()
        .iter()
        .cloned()
        .map(NativeValue::Str)
        .collect();
    ctx.return_native(NativeValue::List(items))
}

/// `617` `__env_get(key: String) -> ?String`
pub fn env_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let key = ctx.arg_string(0)?;
    if key.is_empty() || key.contains(['=', '\0']) {
        ctx.return_null();
        return Ok(());
    }
    match std::env::var(&key) {
        Ok(val) => {
            ctx.check_string_size(val.len())?;
            ctx.return_string(&val)
        }
        Err(_) => {
            ctx.return_null();
            Ok(())
        }
    }
}

/// `618` `__env_vars() -> Map`
pub fn env_vars(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let entries: Vec<(String, NativeValue)> = std::env::vars()
        .map(|(k, v)| (k, NativeValue::Str(v)))
        .collect();
    ctx.return_native(NativeValue::StringMap(entries))
}

/// `619` `__env_current_dir() -> (i64, String)`
pub fn env_current_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    match std::env::current_dir() {
        Ok(dir) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(IO_OK),
            NativeValue::Str(dir.to_string_lossy().into_owned()),
        ])),
        Err(err) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(io_error_code(&err)),
            NativeValue::Str(err.to_string()),
        ])),
    }
}

/// `620` `__env_set_current_dir(path: String) -> (i64, String)`
pub fn env_set_current_dir(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let path = ctx.arg_string(0)?;
    match std::env::set_current_dir(&path) {
        Ok(()) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(IO_OK),
            NativeValue::Str(String::new()),
        ])),
        Err(err) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(io_error_code(&err)),
            NativeValue::Str(err.to_string()),
        ])),
    }
}

/// `621` `__env_os() -> String`
pub fn env_os(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_string(std::env::consts::OS)
}

/// `622` `__env_arch() -> String`
pub fn env_arch(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_string(std::env::consts::ARCH)
}

// ---------------------------------------------------------------------------
// 623..=630: Clocks, sleep, calendar breakdown, normalization & strftime
// ---------------------------------------------------------------------------

fn monotonic_origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// `623` `__time_unix_secs() -> i64`
pub fn time_unix_secs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(neg) => -(i64::try_from(neg.duration().as_secs()).unwrap_or(i64::MAX)),
    };
    ctx.return_i64(secs)
}

/// `624` `__time_unix_millis() -> i64`
pub fn time_unix_millis(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(neg) => -(i64::try_from(neg.duration().as_millis()).unwrap_or(i64::MAX)),
    };
    ctx.return_i64(millis)
}

/// `625` `__time_monotonic_nanos() -> i64`
pub fn time_monotonic_nanos(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let elapsed = monotonic_origin().elapsed().as_nanos();
    let nanos = i64::try_from(elapsed).unwrap_or(i64::MAX);
    ctx.return_i64(nanos)
}

/// `626` `__time_cpu_secs() -> f64`
pub fn time_cpu_secs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_f64(process_cpu_seconds())
}

fn process_cpu_seconds() -> f64 {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[repr(C)]
        struct Timespec {
            tv_sec: std::ffi::c_long,
            tv_nsec: std::ffi::c_long,
        }
        // SAFETY: `clock_gettime` is a standard POSIX / libc symbol accepting a clock ID
        // and a valid writable `struct timespec` pointer on Linux and macOS.
        unsafe extern "C" {
            fn clock_gettime(clk_id: std::ffi::c_int, tp: *mut Timespec) -> std::ffi::c_int;
        }
        #[cfg(target_os = "linux")]
        const CLOCK_PROCESS_CPUTIME_ID: std::ffi::c_int = 2;
        #[cfg(target_os = "macos")]
        const CLOCK_PROCESS_CPUTIME_ID: std::ffi::c_int = 12;

        let mut ts = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, aligned local `timespec` out-pointer and
        // `CLOCK_PROCESS_CPUTIME_ID` is the OS-defined process CPU clock ID.
        let rc = unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
        if rc == 0 {
            return (ts.tv_sec as f64) + (ts.tv_nsec as f64) * 1e-9;
        }
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct FileTime {
            low_date_time: u32,
            high_date_time: u32,
        }
        impl FileTime {
            fn to_u64(self) -> u64 {
                (u64::from(self.high_date_time) << 32) | u64::from(self.low_date_time)
            }
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn GetProcessTimes(
                process: *mut std::ffi::c_void,
                creation_time: *mut FileTime,
                exit_time: *mut FileTime,
                kernel_time: *mut FileTime,
                user_time: *mut FileTime,
            ) -> i32;
        }
        let zero = FileTime {
            low_date_time: 0,
            high_date_time: 0,
        };
        let mut creation = zero;
        let mut exit = zero;
        let mut kernel = zero;
        let mut user = zero;
        // SAFETY: `GetProcessTimes` writes 100-ns ticks into stack-allocated `FileTime` structs.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &raw mut creation,
                &raw mut exit,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if ok != 0 {
            let total_100ns = kernel.to_u64().saturating_add(user.to_u64());
            return (total_100ns as f64) * 1e-7;
        }
    }
    monotonic_origin().elapsed().as_secs_f64()
}

/// `627` `__time_sleep_ms(ms: i64) -> Unit`
pub fn time_sleep_ms(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let ms = ctx.arg_i64(0)?;
    if ms < 0 {
        return Err(VmError::InvalidArgument(
            "sleep duration must not be negative",
        ));
    }
    ctx.sleep_async_ms(ms as u64)
}

#[derive(Clone)]
struct BrokenDownTime {
    year: i64,
    month: i64,      // 1..=12
    day: i64,        // 1..=31
    hour: i64,       // 0..=23
    min: i64,        // 0..=59
    sec: i64,        // 0..=60
    wday_sun1: i64,  // 1..=7 (Sunday = 1)
    yday_1_366: i64, // 1..=366
    is_dst: bool,
    gmtoff_secs: i64,
    tz_abbrev: String,
}

#[derive(Clone, Copy)]
struct CivilDateTimeInput {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    min: i64,
    sec: i64,
    is_dst: i64,
}

#[cfg(all(
    unix,
    target_pointer_width = "64",
    any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    )
))]
mod unix_time {
    use std::ffi::{CStr, c_char, c_int, c_long};

    use super::{BrokenDownTime, CivilDateTimeInput};

    #[repr(C)]
    pub(super) struct LibcTm {
        pub tm_sec: c_int,
        pub tm_min: c_int,
        pub tm_hour: c_int,
        pub tm_mday: c_int,
        pub tm_mon: c_int,
        pub tm_year: c_int,
        pub tm_wday: c_int,
        pub tm_yday: c_int,
        pub tm_isdst: c_int,
        pub tm_gmtoff: c_long,
        pub tm_zone: *const c_char,
    }

    // SAFETY: On 64-bit Linux, macOS, and BSD targets gated above, `time_t` is
    // `c_long` (`i64`) and `struct tm` includes the trailing `tm_gmtoff` and
    // `tm_zone` fields matching `LibcTm` above.
    unsafe extern "C" {
        fn localtime_r(timep: *const c_long, result: *mut LibcTm) -> *mut LibcTm;
        fn mktime(tm: *mut LibcTm) -> c_long;
    }

    pub(super) fn breakdown_local_libc(unix_secs: i64) -> Option<BrokenDownTime> {
        let t = c_long::try_from(unix_secs).ok()?;
        let mut tm = LibcTm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        };
        // SAFETY: `t` and `tm` are valid local stack variables; thread-safe `localtime_r`
        // writes exclusively into `tm` without mutating shared global buffers.
        let ptr = unsafe { localtime_r(&t, &mut tm) };
        if ptr.is_null() {
            return None;
        }
        let tz_abbrev = if !tm.tm_zone.is_null() {
            // SAFETY: `localtime_r` populates `tm_zone` with a static NUL-terminated string.
            unsafe { CStr::from_ptr(tm.tm_zone) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        Some(BrokenDownTime {
            year: i64::from(tm.tm_year) + 1900,
            month: i64::from(tm.tm_mon) + 1,
            day: i64::from(tm.tm_mday),
            hour: i64::from(tm.tm_hour),
            min: i64::from(tm.tm_min),
            sec: i64::from(tm.tm_sec),
            wday_sun1: i64::from(tm.tm_wday) + 1,
            yday_1_366: i64::from(tm.tm_yday) + 1,
            is_dst: tm.tm_isdst > 0,
            gmtoff_secs: tm.tm_gmtoff,
            tz_abbrev,
        })
    }

    pub(super) fn mktime_local(input: CivilDateTimeInput) -> Option<i64> {
        let tm_year = c_int::try_from(input.year.checked_sub(1900)?).ok()?;
        let tm_mon = c_int::try_from(input.month.checked_sub(1)?).ok()?;
        let tm_mday = c_int::try_from(input.day).ok()?;
        let tm_hour = c_int::try_from(input.hour).ok()?;
        let tm_min = c_int::try_from(input.min).ok()?;
        let tm_sec = c_int::try_from(input.sec).ok()?;
        let tm_isdst = input.is_dst.clamp(-1, 1) as c_int;

        let mut tm = LibcTm {
            tm_sec,
            tm_min,
            tm_hour,
            tm_mday,
            tm_mon,
            tm_year,
            // Sentinel: POSIX `mktime` leaves `tm_wday` untouched (-1) on failure,
            // distinguishing error `-1` from valid `1969-12-31 23:59:59 UTC`.
            tm_wday: -1,
            tm_yday: 0,
            tm_isdst,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        };
        // SAFETY: `tm` is a fully initialized local `struct tm` passed to `mktime`.
        let res = unsafe { mktime(&mut tm) };
        if res == -1 && tm.tm_wday == -1 {
            None
        } else {
            Some(res)
        }
    }
}

#[cfg(windows)]
mod win_time {
    use std::ffi::c_int;

    use super::{BrokenDownTime, CivilDateTimeInput, make_unix_utc};

    #[repr(C)]
    pub(super) struct WinTm {
        pub tm_sec: c_int,
        pub tm_min: c_int,
        pub tm_hour: c_int,
        pub tm_mday: c_int,
        pub tm_mon: c_int,
        pub tm_year: c_int,
        pub tm_wday: c_int,
        pub tm_yday: c_int,
        pub tm_isdst: c_int,
    }

    // SAFETY: Standard Windows UCRT 64-bit time conversion routines (`api-ms-win-crt-time-l1-1-0`).
    unsafe extern "C" {
        fn _localtime64_s(tm_out: *mut WinTm, timep: *const i64) -> c_int;
        fn _mktime64(tm: *mut WinTm) -> i64;
    }

    pub(super) fn breakdown_local_win(unix_secs: i64) -> Option<BrokenDownTime> {
        if unix_secs < 0 {
            return None;
        }
        let mut tm = WinTm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
        };
        // SAFETY: `tm` and `unix_secs` are valid stack variables; `_localtime64_s` is thread-safe.
        let rc = unsafe { _localtime64_s(&raw mut tm, &raw const unix_secs) };
        if rc != 0 {
            return None;
        }
        let year = i64::from(tm.tm_year) + 1900;
        let month = i64::from(tm.tm_mon) + 1;
        let day = i64::from(tm.tm_mday);
        let hour = i64::from(tm.tm_hour);
        let min = i64::from(tm.tm_min);
        let sec = i64::from(tm.tm_sec);
        let local_as_utc = make_unix_utc(CivilDateTimeInput {
            year,
            month,
            day,
            hour,
            min,
            sec: sec.min(59),
            is_dst: 0,
        })
        .unwrap_or(unix_secs);
        let gmtoff_secs = local_as_utc.saturating_sub(unix_secs);
        Some(BrokenDownTime {
            year,
            month,
            day,
            hour,
            min,
            sec,
            wday_sun1: i64::from(tm.tm_wday) + 1,
            yday_1_366: i64::from(tm.tm_yday) + 1,
            is_dst: tm.tm_isdst > 0,
            gmtoff_secs,
            tz_abbrev: String::new(),
        })
    }

    pub(super) fn mktime_local_win(input: CivilDateTimeInput) -> Option<i64> {
        let tm_year = c_int::try_from(input.year.checked_sub(1900)?).ok()?;
        let tm_mon = c_int::try_from(input.month.checked_sub(1)?).ok()?;
        let tm_mday = c_int::try_from(input.day).ok()?;
        let tm_hour = c_int::try_from(input.hour).ok()?;
        let tm_min = c_int::try_from(input.min).ok()?;
        let tm_sec = c_int::try_from(input.sec).ok()?;
        let tm_isdst = input.is_dst.clamp(-1, 1) as c_int;

        let mut tm = WinTm {
            tm_sec,
            tm_min,
            tm_hour,
            tm_mday,
            tm_mon,
            tm_year,
            tm_wday: -1,
            tm_yday: 0,
            tm_isdst,
        };
        // SAFETY: `tm` is a fully initialized local `struct tm` passed to `_mktime64`.
        let res = unsafe { _mktime64(&raw mut tm) };
        if res == -1 && tm.tm_wday == -1 {
            None
        } else {
            Some(res)
        }
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Howard Hinnant civil-from-days algorithm (`1970-01-01` -> day `0`).
fn breakdown_civil_utc(unix_secs: i64) -> BrokenDownTime {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3_600;
    let min = (secs_of_day % 3_600) / 60;
    let sec = secs_of_day % 60;

    // 1970-01-01 was a Thursday (Sunday = 0 -> 4).
    let wday0 = (days + 4).rem_euclid(7);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365] from Mar 1
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    if m <= 2 {
        y += 1;
    }

    let jan1_days = days_from_civil(y, 1, 1);
    let yday_1_366 = (days - jan1_days) + 1;

    BrokenDownTime {
        year: y,
        month: m,
        day: d,
        hour,
        min,
        sec,
        wday_sun1: wday0 + 1,
        yday_1_366,
        is_dst: false,
        gmtoff_secs: 0,
        tz_abbrev: "UTC".to_string(),
    }
}

/// Howard Hinnant days-from-civil algorithm (`1970-01-01` -> `0`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn breakdown_timestamp(unix_secs: i64, utc: bool) -> BrokenDownTime {
    if utc {
        return breakdown_civil_utc(unix_secs);
    }
    #[cfg(all(
        unix,
        target_pointer_width = "64",
        any(
            target_os = "linux",
            target_os = "macos",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        )
    ))]
    {
        if let Some(bt) = unix_time::breakdown_local_libc(unix_secs) {
            return bt;
        }
    }
    #[cfg(windows)]
    {
        if let Some(bt) = win_time::breakdown_local_win(unix_secs) {
            return bt;
        }
    }
    breakdown_civil_utc(unix_secs)
}

fn make_unix_utc(input: CivilDateTimeInput) -> Option<i64> {
    let month_zero = i128::from(input.month) - 1;
    let norm_year = i128::from(input.year) + month_zero.div_euclid(12);
    let norm_month = month_zero.rem_euclid(12) + 1;
    let norm_year_i64 = i64::try_from(norm_year).ok()?;
    let norm_month_i64 = norm_month as i64;

    let first_of_month_days = i128::from(days_from_civil(norm_year_i64, norm_month_i64, 1));
    let total_days = first_of_month_days.checked_add(i128::from(input.day) - 1)?;
    let total_secs = total_days
        .checked_mul(86_400)?
        .checked_add(i128::from(input.hour).checked_mul(3_600)?)?
        .checked_add(i128::from(input.min).checked_mul(60)?)?
        .checked_add(i128::from(input.sec))?;
    i64::try_from(total_secs).ok()
}

/// `628` `__time_breakdown(unix_secs: i64, utc: bool) -> (i64, i64, i64, i64, i64, i64, i64, i64, bool)`
pub fn time_breakdown(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let unix_secs = ctx.arg_i64(0)?;
    let utc = ctx.arg_bool(1)?;
    let bt = breakdown_timestamp(unix_secs, utc);
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::I64(bt.year),
        NativeValue::I64(bt.month),
        NativeValue::I64(bt.day),
        NativeValue::I64(bt.hour),
        NativeValue::I64(bt.min),
        NativeValue::I64(bt.sec),
        NativeValue::I64(bt.wday_sun1),
        NativeValue::I64(bt.yday_1_366),
        NativeValue::Bool(bt.is_dst),
    ]))
}

/// `629` `__time_make_unix(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64, is_dst: i64, utc: bool) -> ?i64`
pub fn time_make_unix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(8)?;
    let input = CivilDateTimeInput {
        year: ctx.arg_i64(0)?,
        month: ctx.arg_i64(1)?,
        day: ctx.arg_i64(2)?,
        hour: ctx.arg_i64(3)?,
        min: ctx.arg_i64(4)?,
        sec: ctx.arg_i64(5)?,
        is_dst: ctx.arg_i64(6)?,
    };
    let utc = ctx.arg_bool(7)?;

    let res = if utc {
        make_unix_utc(input)
    } else {
        #[cfg(all(
            unix,
            target_pointer_width = "64",
            any(
                target_os = "linux",
                target_os = "macos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd",
                target_os = "dragonfly",
            )
        ))]
        {
            unix_time::mktime_local(input)
        }
        #[cfg(windows)]
        {
            win_time::mktime_local_win(input)
        }
        #[cfg(not(any(
            windows,
            all(
                unix,
                target_pointer_width = "64",
                any(
                    target_os = "linux",
                    target_os = "macos",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd",
                    target_os = "dragonfly",
                )
            )
        )))]
        {
            make_unix_utc(input)
        }
    };

    ctx.return_native(NativeValue::OptI64(res))
}

/// `630` `__time_strftime(fmt: String, unix_secs: i64, utc: bool) -> (i64, String)`
pub fn time_strftime(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let fmt = ctx.arg_string(0)?;
    let unix_secs = ctx.arg_i64(1)?;
    let utc = ctx.arg_bool(2)?;

    let bt = breakdown_timestamp(unix_secs, utc);
    match format_strftime(&bt, unix_secs, &fmt, ctx)? {
        Ok(rendered) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(0),
            NativeValue::Str(rendered),
        ])),
        Err(err_msg) => ctx.return_native(NativeValue::Tuple(vec![
            NativeValue::I64(1),
            NativeValue::Str(err_msg),
        ])),
    }
}

const SHORT_WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const FULL_WDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const FULL_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn iso_week_year_and_num(year: i64, yday_1_366: i64, wday_sun1: i64) -> (i64, i64) {
    let iso_wday = if wday_sun1 == 1 { 7 } else { wday_sun1 - 1 }; // Mon=1..Sun=7
    let raw_week = (yday_1_366 - iso_wday + 10) / 7;
    if raw_week < 1 {
        let prev_year = year - 1;
        let prev_days = if is_leap_year(prev_year) { 366 } else { 365 };
        // Dec 31 of `prev_year` has weekday `(iso_wday - yday_1_366)` mod 7.
        let dec31_wday = (iso_wday - yday_1_366 - 1).rem_euclid(7) + 1;
        let prev_week = (prev_days - dec31_wday + 10) / 7;
        (prev_year, prev_week)
    } else {
        let year_days = if is_leap_year(year) { 366 } else { 365 };
        let dec31_wday = (iso_wday + (year_days - yday_1_366) - 1).rem_euclid(7) + 1;
        if raw_week == 53 && dec31_wday < 4 {
            (year + 1, 1)
        } else {
            (year, raw_week)
        }
    }
}

fn format_year_4(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        year.to_string()
    }
}

fn format_strftime(
    bt: &BrokenDownTime,
    unix_secs: i64,
    fmt: &str,
    ctx: &BuiltinCtx<'_>,
) -> Result<Result<String, String>, VmError> {
    let mut out = String::with_capacity(fmt.len() + 16);
    let bytes = fmt.as_bytes();
    let mut i = 0usize;

    let wday_idx = ((bt.wday_sun1 - 1).clamp(0, 6)) as usize;
    let mon_idx = ((bt.month - 1).clamp(0, 11)) as usize;
    let hour12 = match bt.hour % 12 {
        0 => 12,
        h => h,
    };
    let am_pm = if bt.hour < 12 { "AM" } else { "PM" };

    while i < bytes.len() {
        if bytes[i] != b'%' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'%' {
                i += 1;
            }
            let piece = &fmt[start..i];
            let next_len = out
                .len()
                .checked_add(piece.len())
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.push_str(piece);
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            return Ok(Err(
                "invalid conversion specifier '%' at end of format".to_string()
            ));
        }
        let spec = bytes[i];
        if !spec.is_ascii() {
            return Ok(Err("invalid non-ASCII conversion specifier".to_string()));
        }
        i += 1;

        let piece: String = match spec {
            b'%' => "%".to_string(),
            b'n' => "\n".to_string(),
            b't' => "\t".to_string(),
            b'Y' => format_year_4(bt.year),
            b'C' => format!("{:02}", bt.year.div_euclid(100)),
            b'y' => format!("{:02}", bt.year.rem_euclid(100)),
            b'm' => format!("{:02}", bt.month),
            b'd' => format!("{:02}", bt.day),
            b'e' => format!("{:2}", bt.day),
            b'H' => format!("{:02}", bt.hour),
            b'I' => format!("{hour12:02}"),
            b'M' => format!("{:02}", bt.min),
            b'S' => format!("{:02}", bt.sec),
            b'p' => am_pm.to_string(),
            b'P' => am_pm.to_ascii_lowercase(),
            b'a' => SHORT_WDAYS[wday_idx].to_string(),
            b'A' => FULL_WDAYS[wday_idx].to_string(),
            b'b' | b'h' => SHORT_MONTHS[mon_idx].to_string(),
            b'B' => FULL_MONTHS[mon_idx].to_string(),
            b'c' => format!(
                "{} {} {:2} {:02}:{:02}:{:02} {}",
                SHORT_WDAYS[wday_idx],
                SHORT_MONTHS[mon_idx],
                bt.day,
                bt.hour,
                bt.min,
                bt.sec,
                format_year_4(bt.year)
            ),
            b'x' | b'D' => format!(
                "{:02}/{:02}/{:02}",
                bt.month,
                bt.day,
                bt.year.rem_euclid(100)
            ),
            b'X' | b'T' => format!("{:02}:{:02}:{:02}", bt.hour, bt.min, bt.sec),
            b'R' => format!("{:02}:{:02}", bt.hour, bt.min),
            b'r' => format!("{hour12:02}:{:02}:{:02} {am_pm}", bt.min, bt.sec),
            b'F' => format!("{}-{:02}-{:02}", format_year_4(bt.year), bt.month, bt.day),
            b'w' => (bt.wday_sun1 - 1).to_string(),
            b'u' => (if bt.wday_sun1 == 1 {
                7
            } else {
                bt.wday_sun1 - 1
            })
            .to_string(),
            b'j' => format!("{:03}", bt.yday_1_366),
            b'U' => {
                let yday0 = bt.yday_1_366 - 1;
                let wday0 = bt.wday_sun1 - 1;
                format!("{:02}", (yday0 + 7 - wday0) / 7)
            }
            b'W' => {
                let yday0 = bt.yday_1_366 - 1;
                let mon0 = (bt.wday_sun1 + 5) % 7;
                format!("{:02}", (yday0 + 7 - mon0) / 7)
            }
            b'V' => {
                let (_, iso_w) = iso_week_year_and_num(bt.year, bt.yday_1_366, bt.wday_sun1);
                format!("{iso_w:02}")
            }
            b'G' => {
                let (iso_y, _) = iso_week_year_and_num(bt.year, bt.yday_1_366, bt.wday_sun1);
                format_year_4(iso_y)
            }
            b'g' => {
                let (iso_y, _) = iso_week_year_and_num(bt.year, bt.yday_1_366, bt.wday_sun1);
                format!("{:02}", iso_y.rem_euclid(100))
            }
            b'z' => format_utc_offset(bt.gmtoff_secs),
            b'Z' => {
                if bt.tz_abbrev.is_empty() {
                    format_utc_offset(bt.gmtoff_secs)
                } else {
                    bt.tz_abbrev.clone()
                }
            }
            b's' => unix_secs.to_string(),
            other => {
                return Ok(Err(format!(
                    "invalid conversion specifier '%{}'",
                    other as char
                )));
            }
        };

        let next_len = out
            .len()
            .checked_add(piece.len())
            .ok_or(VmError::DisplaySizeExceeded)?;
        ctx.check_string_size(next_len)?;
        out.push_str(&piece);
    }

    Ok(Ok(out))
}

fn format_utc_offset(gmtoff_secs: i64) -> String {
    let sign = if gmtoff_secs < 0 { '-' } else { '+' };
    let total_mins = gmtoff_secs.unsigned_abs() / 60;
    let hh = total_mins / 60;
    let mm = total_mins % 60;
    format!("{sign}{hh:02}{mm:02}")
}

// ---------------------------------------------------------------------------
// 631..=633: Process execution & PID
// ---------------------------------------------------------------------------

/// `631` `__proc_exec(prog: String, args: List, stdin_data: ?String) -> (i64, i64, String, String)`
pub fn proc_exec(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.spawn_process_async(|ctx| {
        let prog = ctx.arg_string(0)?;
        let args_root = ctx.arg_rooted(1)?;
        let stdin_data = ctx.arg_optional_string(2)?;

        let args_val = ctx.reload_root(&args_root)?;
        let args_len = ctx.list_len(args_val)?;
        let mut args = Vec::with_capacity(args_len);
        for idx in 0..args_len {
            let elem_root = ctx.list_get_rooted(&args_root, idx)?;
            let elem = ctx.reload_root(&elem_root)?;
            let Some(s) = extract_string_arg(elem) else {
                return Err(VmError::TypeError);
            };
            args.push(s);
        }

        Ok(Some(ProcessSpec {
            prog,
            args,
            stdin_data: stdin_data.map(String::into_bytes),
            cwd: None,
        }))
    })
}

/// `646` `__proc_exec_cwd(prog: String, args: List, stdin_data: ?String, cwd: ?String) -> (i64, i64, String, String)`
pub fn proc_exec_cwd(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    ctx.spawn_process_async(|ctx| {
        let prog = ctx.arg_string(0)?;
        let args_root = ctx.arg_rooted(1)?;
        let stdin_data = ctx.arg_optional_string(2)?;
        let cwd = ctx.arg_optional_string(3)?;

        let args_val = ctx.reload_root(&args_root)?;
        let args_len = ctx.list_len(args_val)?;
        let mut args = Vec::with_capacity(args_len);
        for idx in 0..args_len {
            let elem_root = ctx.list_get_rooted(&args_root, idx)?;
            let elem = ctx.reload_root(&elem_root)?;
            let Some(s) = extract_string_arg(elem) else {
                return Err(VmError::TypeError);
            };
            args.push(s);
        }

        Ok(Some(ProcessSpec {
            prog,
            args,
            stdin_data: stdin_data.map(String::into_bytes),
            cwd: cwd.filter(|s| !s.is_empty()),
        }))
    })
}

/// `632` `__proc_shell(cmd: String, stdin_data: ?String) -> (i64, i64, String, String)`
pub fn proc_shell(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.spawn_process_async(|ctx| {
        let cmd = ctx.arg_string(0)?;
        let stdin_data = ctx.arg_optional_string(1)?;

        if cmd.is_empty() {
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::I64(1),
                NativeValue::Str(String::new()),
                NativeValue::Str(String::new()),
            ]))?;
            return Ok(None);
        }

        #[cfg(windows)]
        let (shell_prog, shell_args) = ("cmd".to_string(), vec!["/C".to_string(), cmd]);
        #[cfg(not(windows))]
        let (shell_prog, shell_args) = ("sh".to_string(), vec!["-c".to_string(), cmd]);

        Ok(Some(ProcessSpec {
            prog: shell_prog,
            args: shell_args,
            stdin_data: stdin_data.map(String::into_bytes),
            cwd: None,
        }))
    })
}

/// `647` `__proc_shell_cwd(cmd: String, stdin_data: ?String, cwd: ?String) -> (i64, i64, String, String)`
pub fn proc_shell_cwd(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    ctx.spawn_process_async(|ctx| {
        let cmd = ctx.arg_string(0)?;
        let stdin_data = ctx.arg_optional_string(1)?;
        let cwd = ctx.arg_optional_string(2)?;

        if cmd.is_empty() {
            ctx.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(IO_OK),
                NativeValue::I64(1),
                NativeValue::Str(String::new()),
                NativeValue::Str(String::new()),
            ]))?;
            return Ok(None);
        }

        #[cfg(windows)]
        let (shell_prog, shell_args) = ("cmd".to_string(), vec!["/C".to_string(), cmd]);
        #[cfg(not(windows))]
        let (shell_prog, shell_args) = ("sh".to_string(), vec!["-c".to_string(), cmd]);

        Ok(Some(ProcessSpec {
            prog: shell_prog,
            args: shell_args,
            stdin_data: stdin_data.map(String::into_bytes),
            cwd: cwd.filter(|s| !s.is_empty()),
        }))
    })
}

/// `633` `__proc_pid() -> i64`
pub fn proc_pid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_i64(i64::from(std::process::id()))
}

fn extract_string_arg(val: runtime::TaggedValue) -> Option<String> {
    let ptr = val.as_heap_ptr()?;
    // SAFETY: `val` was loaded from a live `BuiltinRoot` within this native call.
    let bytes = unsafe { gc::heap_string_bytes(ptr) }?;
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

// ---------------------------------------------------------------------------
// 634..=637: PRNG, GC collection & task backtrace
// ---------------------------------------------------------------------------

/// `634` `__rng_next_u64() -> u64`
pub fn rng_next_u64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let val = ctx.rng_next_u64();
    ctx.return_number(Number::U64(val))
}

/// `635` `__rng_seed(mode: i64, s1: i64, s2: i64) -> (i64, i64)`
pub fn rng_seed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let mode = ctx.arg_i64(0)?;
    let s1 = ctx.arg_i64(1)?;
    let s2 = ctx.arg_i64(2)?;
    let (a, b) = ctx.rng_seed(mode, s1, s2)?;
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::I64(a),
        NativeValue::I64(b),
    ]))
}

/// `636` `__rt_gc_collect(mode: i64) -> i64`
pub fn rt_gc_collect(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let mode = ctx.arg_i64(0)?;
    match mode {
        0 => {
            ctx.collect_garbage()?;
            let epochs = i64::try_from(ctx.completed_collections()).unwrap_or(i64::MAX);
            ctx.return_i64(epochs)
        }
        1 => {
            let epochs = i64::try_from(ctx.completed_collections()).unwrap_or(i64::MAX);
            ctx.return_i64(epochs)
        }
        _ => Err(VmError::InvalidArgument("invalid gc collect mode")),
    }
}

/// `637` `__rt_backtrace(msg: ?String, skip_frames: i64) -> String`
pub fn rt_backtrace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let msg = ctx.arg_optional_string(0)?;
    let skip_raw = ctx.arg_i64(1)?;
    let skip = usize::try_from(skip_raw.max(0)).unwrap_or(usize::MAX);
    let trace = ctx.format_backtrace(msg.as_deref(), skip)?;
    ctx.check_string_size(trace.len())?;
    ctx.return_string(&trace)
}

// ---------------------------------------------------------------------------
// 688..=691: Tiered JIT runtime introspection
// ---------------------------------------------------------------------------

/// `688` `__rt_jit_enabled() -> bool`
pub fn rt_jit_enabled(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_bool(ctx.jit_enabled());
    Ok(())
}

/// `689` `__rt_jit_compiled_fns() -> usize`
pub fn rt_jit_compiled_fns(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let count = ctx.jit_stats().compiled_functions as u64;
    ctx.return_number(Number::U64(count))
}

/// `690` `__rt_jit_code_bytes() -> usize`
pub fn rt_jit_code_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let bytes = ctx.jit_stats().code_bytes as u64;
    ctx.return_number(Number::U64(bytes))
}

/// `691` `__rt_jit_deopt_count() -> usize`
pub fn rt_jit_deopt_count(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    let count = ctx.jit_stats().deopt_count as u64;
    ctx.return_number(Number::U64(count))
}
