//! The processes a CLI started, so a session can end them all.
//!
//! The CLI starts hooks and Bash tools in process groups of their own
//! (Node `detached: true`), so signalling the CLI's group misses them.
//! While the CLI runs they are its descendants: `descendant_groups` walks
//! the parent links of a process-table snapshot. A process that detached
//! and outlived its parent (`(cmd &)` in a hook) has no link left and is
//! out of reach.
#![cfg(unix)]

use std::collections::HashSet;

/// One row of the process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Proc {
    pub(crate) pid: i32,
    pub(crate) ppid: i32,
    pub(crate) pgid: i32,
}

/// Process groups of `root`'s descendants, without `root`'s own group,
/// `exclude` (the host's group) or the system groups 0 and 1.
pub(crate) fn descendant_groups(table: &[Proc], root: i32, exclude: i32) -> Vec<i32> {
    let mut groups = vec![];
    let mut seen = HashSet::from([root]);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for p in table.iter().filter(|p| p.ppid == parent) {
            if !seen.insert(p.pid) {
                continue;
            }
            frontier.push(p.pid);
            if p.pgid > 1 && p.pgid != root && p.pgid != exclude && !groups.contains(&p.pgid) {
                groups.push(p.pgid);
            }
        }
    }
    groups
}

/// Rows of `ps -A -o pid=,ppid=,pgid=` output; malformed lines are skipped.
pub(crate) fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|line| {
            let mut n = line.split_whitespace().map(str::parse::<i32>);
            match (n.next(), n.next(), n.next()) {
                (Some(Ok(pid)), Some(Ok(ppid)), Some(Ok(pgid))) => Some(Proc { pid, ppid, pgid }),
                _ => None,
            }
        })
        .collect()
}

/// The row of a Linux `/proc/<pid>/stat` line; `comm` may hold spaces and
/// parentheses, so fields are counted from the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_stat(stat: &str) -> Option<Proc> {
    let pid = stat.split_whitespace().next()?.parse().ok()?;
    let mut rest = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    let pgid = rest.next()?.parse().ok()?;
    Some(Proc { pid, ppid, pgid })
}

/// The process table of this machine; empty when it cannot be read.
/// Blocking: Linux reads `/proc`, elsewhere this runs `ps`.
pub(crate) fn snapshot() -> Vec<Proc> {
    #[cfg(target_os = "linux")]
    if let Ok(entries) = std::fs::read_dir("/proc") {
        return entries
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
            .filter_map(|stat| parse_proc_stat(&stat))
            .collect();
    }
    let ps = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match ps {
        Ok(out) if out.status.success() => parse_ps(&String::from_utf8_lossy(&out.stdout)),
        other => {
            tracing::debug!(?other, "could not read the process table");
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Linux path cannot run on the macOS dev machine; CI runs it.
    #[test]
    fn reads_proc_stat_with_odd_comm() {
        let row = parse_proc_stat("123 (a) b (c)) S 45 67 67 0 -1");
        assert_eq!(row, Some(Proc { pid: 123, ppid: 45, pgid: 67 }));
    }
}
