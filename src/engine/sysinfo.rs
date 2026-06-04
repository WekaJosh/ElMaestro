//! Client hardware / OS fact gathering.
//!
//! Runs one POSIX-sh probe script on each client (locally for the
//! coordinator, over SSH for workers) and parses its TSV output into a
//! `SystemInfo`. Everything degrades gracefully: a missing tool or a
//! root-only fact (DIMM speed) just leaves that field empty. Gathering
//! never fails a benchmark run.
//!
//! Results are cached per host for the lifetime of the process: hardware
//! doesn't change between specs in a sweep, so we probe each host once.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;

use crate::config::ClientHost;
use crate::engine::ssh::SshRunner;
use crate::results::schema::{NicInfo, SystemInfo};

/// POSIX sh probe. Emits TAB-separated `KEY\tVALUE` lines. Linux-first,
/// with sysctl/sw_vers fallbacks so a macOS coordinator returns
/// something useful too. Each fact is independent: any failure prints
/// nothing for that key rather than aborting.
const PROBE: &str = r#"
uname -s 2>/dev/null | sed 's/^/KERNEL_SYS\t/'
uname -r 2>/dev/null | sed 's/^/KERNEL_REL\t/'
if [ -r /etc/os-release ]; then
  ( . /etc/os-release 2>/dev/null; printf 'OS\t%s\n' "$PRETTY_NAME" )
elif command -v sw_vers >/dev/null 2>&1; then
  printf 'OS\t%s %s\n' "$(sw_vers -productName 2>/dev/null)" "$(sw_vers -productVersion 2>/dev/null)"
fi
if [ -r /proc/cpuinfo ]; then
  printf 'CPU_MODEL\t%s\n' "$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^[ \t]*//')"
  printf 'CPU_COUNT\t%s\n' "$(grep -c '^processor' /proc/cpuinfo)"
elif command -v sysctl >/dev/null 2>&1; then
  printf 'CPU_MODEL\t%s\n' "$(sysctl -n machdep.cpu.brand_string 2>/dev/null)"
  printf 'CPU_COUNT\t%s\n' "$(sysctl -n hw.ncpu 2>/dev/null)"
fi
if [ -r /proc/meminfo ]; then
  printf 'MEM_TOTAL_KB\t%s\n' "$(awk '/^MemTotal/{print $2}' /proc/meminfo)"
elif command -v sysctl >/dev/null 2>&1; then
  printf 'MEM_TOTAL_BYTES\t%s\n' "$(sysctl -n hw.memsize 2>/dev/null)"
fi
if command -v dmidecode >/dev/null 2>&1; then
  spd=$(dmidecode -t 17 2>/dev/null | awk -F: '/Configured Memory Speed|Configured Clock Speed/{gsub(/^[ \t]+/,"",$2); if ($2 !~ /Unknown/ && $2 != "") {print $2; exit}}')
  [ -z "$spd" ] && spd=$(dmidecode -t 17 2>/dev/null | awk -F: '/[ \t]Speed:/{gsub(/^[ \t]+/,"",$2); if ($2 !~ /Unknown/ && $2 != "") {print $2; exit}}')
  [ -n "$spd" ] && printf 'MEM_SPEED\t%s\n' "$spd"
  typ=$(dmidecode -t 17 2>/dev/null | awk -F: '/^[ \t]*Type:/{gsub(/^[ \t]+/,"",$2); if ($2 !~ /Unknown/ && $2 != "") {print $2; exit}}')
  [ -n "$typ" ] && printf 'MEM_TYPE\t%s\n' "$typ"
fi
if [ -d /sys/class/net ]; then
  for d in /sys/class/net/*; do
    ifc=$(basename "$d")
    [ "$ifc" = "lo" ] && continue
    sp=$(cat "$d/speed" 2>/dev/null)
    case "$sp" in ''|*[!0-9]*) continue ;; esac
    [ "$sp" -gt 0 ] 2>/dev/null && printf 'NIC\t%s\t%s\n' "$ifc" "$sp"
  done
fi
"#;

static CACHE: Lazy<Mutex<HashMap<String, SystemInfo>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn cache_get(host: &str) -> Option<SystemInfo> {
    CACHE.lock().ok().and_then(|m| m.get(host).cloned())
}

fn cache_put(host: &str, info: &SystemInfo) {
    if let Ok(mut m) = CACHE.lock() {
        m.insert(host.to_string(), info.clone());
    }
}

/// Gather facts for a client. Local hosts run the probe directly; remote
/// hosts run it over SSH. Cached by host. Returns None only if the probe
/// produced nothing parseable at all (the run still proceeds).
pub async fn gather(client: &ClientHost) -> Option<SystemInfo> {
    if let Some(cached) = cache_get(&client.host) {
        return Some(cached);
    }
    let raw = if is_local(&client.host) {
        gather_local_raw()
    } else {
        gather_remote_raw(client).await
    }?;
    let info = parse_probe(&raw);
    if info_is_empty(&info) {
        return None;
    }
    cache_put(&client.host, &info);
    Some(info)
}

/// Synchronous local-only gather, for the non-async run_locally path.
/// Cached by host like the async version.
pub fn gather_local_blocking(host: &str) -> Option<SystemInfo> {
    if let Some(cached) = cache_get(host) {
        return Some(cached);
    }
    let raw = gather_local_raw()?;
    let info = parse_probe(&raw);
    if info_is_empty(&info) {
        return None;
    }
    cache_put(host, &info);
    Some(info)
}

fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "")
}

fn gather_local_raw() -> Option<String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(PROBE)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn gather_remote_raw(client: &ClientHost) -> Option<String> {
    let runner = SshRunner::new(client.clone());
    let r = runner
        .run(&["sh", "-c", PROBE], Some(Duration::from_secs(15)))
        .await
        .ok()?;
    if r.stdout.trim().is_empty() {
        return None;
    }
    Some(r.stdout)
}

fn info_is_empty(i: &SystemInfo) -> bool {
    i.cpu_model.is_none()
        && i.cpu_count.is_none()
        && i.mem_total_bytes.is_none()
        && i.os.is_none()
        && i.kernel.is_none()
        && i.nics.is_empty()
}

/// Parse TSV probe output into a SystemInfo. Unknown keys are ignored;
/// malformed values are skipped (the field stays None).
fn parse_probe(raw: &str) -> SystemInfo {
    let mut info = SystemInfo::default();
    let mut kernel_sys: Option<String> = None;
    let mut kernel_rel: Option<String> = None;

    for line in raw.lines() {
        let mut it = line.splitn(3, '\t');
        let key = it.next().unwrap_or("");
        let v1 = it.next().unwrap_or("").trim();
        let v2 = it.next().unwrap_or("").trim();
        if v1.is_empty() && key != "NIC" {
            continue;
        }
        match key {
            "KERNEL_SYS" => kernel_sys = Some(v1.to_string()),
            "KERNEL_REL" => kernel_rel = Some(v1.to_string()),
            "OS" => info.os = Some(v1.to_string()),
            "CPU_MODEL" => info.cpu_model = Some(v1.to_string()),
            "CPU_COUNT" => info.cpu_count = v1.parse().ok(),
            "MEM_TOTAL_KB" => info.mem_total_bytes = v1.parse::<u64>().ok().map(|kb| kb * 1024),
            "MEM_TOTAL_BYTES" => info.mem_total_bytes = v1.parse().ok(),
            "MEM_SPEED" => info.mem_speed = Some(v1.to_string()),
            "MEM_TYPE" => info.mem_type = Some(v1.to_string()),
            "NIC" => {
                if let Ok(speed) = v2.parse::<u64>() {
                    if !v1.is_empty() {
                        info.nics.push(NicInfo {
                            name: v1.to_string(),
                            speed_mbps: speed,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // Combine "Linux" + "5.15.0-x" into one kernel string.
    info.kernel = match (kernel_sys, kernel_rel) {
        (Some(s), Some(r)) => Some(format!("{} {}", s, r)),
        (Some(s), None) => Some(s),
        (None, Some(r)) => Some(r),
        (None, None) => None,
    };
    info
}

/// Human-friendly memory size, e.g. "503.5 GiB". Used by the report
/// layers so they don't each reimplement byte formatting.
pub fn fmt_mem(bytes: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const TIB: f64 = (1u64 << 40) as f64;
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.2} TiB", b / TIB)
    } else {
        format!("{:.1} GiB", b / GIB)
    }
}

/// Human-friendly NIC speed, e.g. "100 GbE", "25 GbE", "1 GbE", else Mbps.
pub fn fmt_nic_speed(mbps: u64) -> String {
    if mbps >= 1000 && mbps % 1000 == 0 {
        format!("{} GbE", mbps / 1000)
    } else if mbps >= 1000 {
        format!("{:.1} GbE", mbps as f64 / 1000.0)
    } else {
        format!("{} Mb/s", mbps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_facts() {
        let raw = "KERNEL_SYS\tLinux\n\
                   KERNEL_REL\t5.15.0-89-generic\n\
                   OS\tUbuntu 22.04.3 LTS\n\
                   CPU_MODEL\tAMD EPYC 7763 64-Core Processor\n\
                   CPU_COUNT\t128\n\
                   MEM_TOTAL_KB\t527966540\n\
                   MEM_SPEED\t3200 MT/s\n\
                   MEM_TYPE\tDDR4\n\
                   NIC\teth0\t100000\n\
                   NIC\tib0\t200000\n";
        let i = parse_probe(raw);
        assert_eq!(i.cpu_model.as_deref(), Some("AMD EPYC 7763 64-Core Processor"));
        assert_eq!(i.cpu_count, Some(128));
        assert_eq!(i.mem_total_bytes, Some(527966540 * 1024));
        assert_eq!(i.mem_speed.as_deref(), Some("3200 MT/s"));
        assert_eq!(i.mem_type.as_deref(), Some("DDR4"));
        assert_eq!(i.os.as_deref(), Some("Ubuntu 22.04.3 LTS"));
        assert_eq!(i.kernel.as_deref(), Some("Linux 5.15.0-89-generic"));
        assert_eq!(i.nics.len(), 2);
        assert_eq!(i.nics[0].name, "eth0");
        assert_eq!(i.nics[0].speed_mbps, 100000);
        assert_eq!(i.nics[1].name, "ib0");
        assert_eq!(i.nics[1].speed_mbps, 200000);
    }

    #[test]
    fn skips_malformed_and_unknown_lines() {
        let raw = "GARBAGE\tvalue\n\
                   CPU_COUNT\tnotanumber\n\
                   NIC\teth0\tnotaspeed\n\
                   CPU_MODEL\tXeon\n";
        let i = parse_probe(raw);
        assert_eq!(i.cpu_count, None);
        assert!(i.nics.is_empty());
        assert_eq!(i.cpu_model.as_deref(), Some("Xeon"));
    }

    #[test]
    fn macos_style_bytes_and_pretty_name() {
        let raw = "KERNEL_SYS\tDarwin\n\
                   OS\tmacOS 14.4\n\
                   CPU_MODEL\tApple M3 Max\n\
                   CPU_COUNT\t16\n\
                   MEM_TOTAL_BYTES\t137438953472\n";
        let i = parse_probe(raw);
        assert_eq!(i.mem_total_bytes, Some(137438953472));
        assert_eq!(i.kernel.as_deref(), Some("Darwin"));
        assert_eq!(i.cpu_model.as_deref(), Some("Apple M3 Max"));
    }

    #[test]
    fn empty_detection() {
        assert!(info_is_empty(&SystemInfo::default()));
        let mut i = SystemInfo::default();
        i.cpu_count = Some(4);
        assert!(!info_is_empty(&i));
    }

    #[test]
    fn nic_speed_formatting() {
        assert_eq!(fmt_nic_speed(100000), "100 GbE");
        assert_eq!(fmt_nic_speed(25000), "25 GbE");
        assert_eq!(fmt_nic_speed(1000), "1 GbE");
        assert_eq!(fmt_nic_speed(2500), "2.5 GbE");
        assert_eq!(fmt_nic_speed(100), "100 Mb/s");
    }

    #[test]
    fn mem_formatting() {
        assert_eq!(fmt_mem(137438953472), "128.0 GiB");
        assert_eq!(fmt_mem(2 * (1u64 << 40)), "2.00 TiB");
    }
}
