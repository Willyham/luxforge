//! Parsers for the two Linux `/proc/self` files the counters read. They are plain text functions,
//! compiled on every platform so their tests run on the owner's Mac as well as in the Linux CI.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// User plus system clock ticks from `/proc/self/stat`: fields 14 and 15, `utime` and `stime`.
/// Field 2 is the command name in parentheses and may itself contain spaces and parentheses, so
/// the count starts after the last `)`, where field 3 begins.
pub(crate) fn stat_ticks(stat: &str) -> Option<u64> {
    let after = &stat[stat.rfind(')')? + 1..];
    let mut fields = after.split_ascii_whitespace();
    let user: u64 = fields.nth(14 - 3)?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    user.checked_add(system)
}

/// One `kB` line of `/proc/self/status`, such as `VmRSS:     81234 kB`, in bytes. The kernel's
/// `kB` is 1024 bytes.
pub(crate) fn status_bytes(status: &str, key: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?;
        let kib: u64 = value.trim().strip_suffix("kB")?.trim_end().parse().ok()?;
        kib.checked_mul(1024)
    })
}

/// Clock ticks to nanoseconds at `per_second` ticks a second, without overflow.
pub(crate) fn ticks_ns(ticks: u64, per_second: u64) -> u64 {
    let ns = u128::from(ticks) * 1_000_000_000 / u128::from(per_second.max(1));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_reads_utime_and_stime_after_a_command_name_with_spaces_and_parentheses() {
        // Fields from a real `/proc/self/stat`, with the command name made awkward.
        let stat = "4242 (my (odd) cmd) R 1 4242 4242 0 -1 4194560 1234 0 0 0 250 75 0 0 20 0 9 0 \
                    123456 987654321 2048 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0";
        assert_eq!(stat_ticks(stat), Some(250 + 75));
        assert_eq!(stat_ticks("4242 (cut short) R 1 2"), None);
        assert_eq!(stat_ticks("no parentheses at all"), None);
    }

    #[test]
    fn status_lines_are_kibibytes_and_keys_match_exactly() {
        let status = "Name:\tluxforge\nVmPeak:\t  900000 kB\nVmHWM:\t  524288 kB\n\
                      VmRSS:\t  262144 kB\nRssAnon:\t  1 kB\n";
        assert_eq!(status_bytes(status, "VmRSS"), Some(262_144 * 1024));
        assert_eq!(status_bytes(status, "VmHWM"), Some(524_288 * 1024));
        assert_eq!(status_bytes(status, "Vm"), None, "a prefix is not a key");
        assert_eq!(status_bytes(status, "VmSwap"), None);
        assert_eq!(status_bytes("VmRSS:\t 12 MB\n", "VmRSS"), None);
    }

    #[test]
    fn ticks_convert_to_nanoseconds_without_overflow() {
        assert_eq!(ticks_ns(250, 100), 2_500_000_000);
        assert_eq!(
            ticks_ns(3, 0),
            3_000_000_000,
            "a zero rate is treated as one"
        );
        assert_eq!(ticks_ns(u64::MAX, 1), u64::MAX);
    }
}
