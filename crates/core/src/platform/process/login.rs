//! What a client needs to tell a daemon left from an earlier login session
//! (macOS): the start of the GUI login session, and of the daemon.

use std::path::Path;

use crate::ps::{ProcessRow, StartTime};

#[cfg(target_os = "macos")]
use tokio::process::Command;

#[cfg(target_os = "macos")]
use super::run_tool;

/// The program that runs a user's GUI login session on macOS, as `ps` shows
/// it. macOS starts a new one for each login, after the last session ended.
const LOGINWINDOW: &str = "/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow";
/// The Dock, which starts with each GUI login session, a little after its
/// first login items. Read only when there is no `loginwindow` of the user:
/// `killall Dock` starts a new one within the same session.
const DOCK: &str = "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock";

/// When the GUI login session of `uid` started: the start of
/// its `loginwindow`, or of its Dock when it has none, the latest if there
/// are several. `None` when `uid` has no GUI session in `table`, as for a
/// login over SSH only.
pub fn login_session_start(table: &[ProcessRow], uid: u32) -> Option<StartTime> {
    [LOGINWINDOW, DOCK].into_iter().find_map(|program| {
        table
            .iter()
            .filter(|row| row.uid == uid && row.command.as_path() == Path::new(program))
            .max_by_key(|row| row.started.utc_seconds())
            .map(|row| row.started)
    })
}

/// When a daemon started, and when the current GUI login session of its user
/// did. Both in UTC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTimes {
    /// When the daemon started.
    pub started: StartTime,
    /// When the login session started.
    pub login: StartTime,
}

/// The [`LoginTimes`] of the `marsad` at `pid`, from `table`. `own` is the
/// pid of the process that asks. `None` unless `pid` was started as a
/// program named `marsad` by the same user as `own`, and that user has a GUI
/// session: then the daemon is never judged.
pub fn login_times(table: &[ProcessRow], pid: u32, own: u32) -> Option<LoginTimes> {
    let row = |pid| table.iter().find(|row| row.pid == pid);
    let daemon = row(pid)?;
    let uid = row(own)?.uid;
    if daemon.uid != uid || daemon.command.file_name()? != "marsad" {
        return None;
    }
    Some(LoginTimes {
        started: daemon.started,
        login: login_session_start(table, uid)?,
    })
}

/// The [`LoginTimes`] of the daemon at `pid` now. macOS reads
/// the process table once with `/bin/ps`, with `LC_ALL=C` and `TZ=UTC0` as
/// [`super::identity`] does, so the times compare. `None` when it cannot be told.
#[cfg(target_os = "macos")]
pub async fn read_login_times(pid: u32) -> Option<LoginTimes> {
    login_times(&process_table().await?, pid, std::process::id())
}

/// Not known on this system: no daemon is replaced for it.
#[cfg(not(target_os = "macos"))]
pub async fn read_login_times(_pid: u32) -> Option<LoginTimes> {
    None
}

/// Every process now, from `/bin/ps`. `None` when the table cannot be read.
#[cfg(target_os = "macos")]
async fn process_table() -> Option<Vec<ProcessRow>> {
    let mut ps = Command::new("/bin/ps");
    ps.args(["-ww", "-axo", "pid=,uid=,lstart=,comm="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");
    let out = run_tool(ps).await?;
    if !out.status.success() {
        return None;
    }
    Some(crate::ps::parse_process_table(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use super::super::at;

    #[cfg(target_os = "macos")]
    use super::super::{after_exec, identity};
    #[cfg(target_os = "macos")]
    use crate::orphan::Liveness;

    /// `ps -axo pid=,uid=,lstart=,comm=` as `LC_ALL=C TZ=UTC0` gives it on
    /// macOS: a real table cut down, with lines added.
    const TABLE: &str = "\
    1     0 Thu Jan  1 00:00:06 1970     /sbin/launchd
  300     0 Wed Nov 11 05:52:40 2026     /System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow
 5793   501 Wed Nov 11 10:12:30 2026     /Users/me/Library/Application Support/dev.marsa.Marsa/bin/marsad
37805   501 Sat Nov 14 13:28:04 2026     /System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow
37949   501 Sat Nov 14 13:28:16 2026     /System/Library/CoreServices/Dock.app/Contents/MacOS/Dock
37986   501 Sat Nov 14 13:28:17 2026     /System/Library/CoreServices/Dock.app/Contents/XPCServices/com.apple.dock.extra.xpc/Contents/MacOS/com.apple.dock.extra
40686   501 Sat Nov 14 13:28:26 2026     /Users/me/Library/Application Support/dev.marsa.Marsa/bin/marsa-tray
40700   501 Sat Nov 14 13:30:00 2026     /fake/loginwindow
40800   501 Sat Nov 14 13:31:00 2026     ./marsad
40900   501 Sat Nov 14 13:31:30 2026     /usr/bin/marsad-helper
41000   502 Sat Nov 14 14:00:00 2026     /System/Library/CoreServices/Dock.app/Contents/MacOS/Dock
41001   502 Sat Nov 14 14:00:09 2026     /Users/other/bin/marsad
41100   503 Sat Nov 14 14:05:00 2026     /System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow
41101   503 Sat Nov 14 14:06:00 2026     /System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow
41102   503 Sat Nov 14 14:07:00 2026     /bin/zsh
41103   503 Sat Nov 14 14:07:01 2026     /Users/third/bin/marsad
41200   504 Sat Nov 14 14:10:00 2026     /bin/zsh
41201   504 Sat Nov 14 14:10:01 2026     /Users/ssh/bin/marsad
42000   501 Sat Nov 14 13:31\x20
x       501 Sat Nov 14 13:31:00 2026     /bin/zsh
42100   501 Sat Nov 14 13:31:00 2026
42200   501 Sat Nov 14 13:31:00 2026     /bin/zsh\r
";

    #[test]
    fn process_table_lines_give_pid_uid_start_and_command() {
        let table = crate::ps::parse_process_table(TABLE);
        let row = |pid| table.iter().find(|row| row.pid == pid).cloned();
        // A command with spaces is kept whole.
        assert_eq!(
            row(5793),
            Some(ProcessRow {
                pid: 5793,
                uid: 501,
                started: at("Wed Nov 11 10:12:30 2026"),
                command: PathBuf::from(
                    "/Users/me/Library/Application Support/dev.marsa.Marsa/bin/marsad"
                ),
            })
        );
        assert_eq!(row(1).map(|row| row.uid), Some(0));
        // `\r` is trimmed.
        assert_eq!(row(42200).map(|row| row.command), Some("/bin/zsh".into()));
        // A cut time and a line without a command are skipped, and so is a
        // pid that is no number.
        for skipped in [42000, 42100] {
            assert_eq!(row(skipped), None, "{skipped}");
        }
        assert_eq!(table.len(), 19, "{table:#?}");
        assert!(crate::ps::parse_process_table("").is_empty());
    }

    #[test]
    fn the_login_session_starts_with_the_users_loginwindow_else_its_dock() {
        let table = crate::ps::parse_process_table(TABLE);
        // This user's own loginwindow: not root's at the login screen, not a
        // process that calls itself loginwindow, and not the Dock.
        assert_eq!(
            login_session_start(&table, 501),
            Some(at("Sat Nov 14 13:28:04 2026"))
        );
        assert_eq!(
            login_session_start(&table, 0),
            Some(at("Wed Nov 11 05:52:40 2026"))
        );
        // No loginwindow of its own: the Dock, never `com.apple.dock.extra`.
        assert_eq!(
            login_session_start(&table, 502),
            Some(at("Sat Nov 14 14:00:00 2026"))
        );
        // Two of them: the latest.
        assert_eq!(
            login_session_start(&table, 503),
            Some(at("Sat Nov 14 14:06:00 2026"))
        );
        // No GUI session, as over SSH.
        assert_eq!(login_session_start(&table, 504), None);
        assert_eq!(login_session_start(&table, 9999), None);
        assert_eq!(login_session_start(&[], 501), None);
    }

    #[test]
    fn login_times_are_read_for_a_marsad_of_the_asking_user_only() {
        let table = crate::ps::parse_process_table(TABLE);
        let times = |started: &str, login: &str| LoginTimes {
            started: at(started),
            login: at(login),
        };
        // The incident: a daemon from the session before.
        assert_eq!(
            login_times(&table, 5793, 40686),
            Some(times(
                "Wed Nov 11 10:12:30 2026",
                "Sat Nov 14 13:28:04 2026"
            ))
        );
        // Started from a terminal as `./marsad`.
        assert_eq!(
            login_times(&table, 40800, 40686),
            Some(times(
                "Sat Nov 14 13:31:00 2026",
                "Sat Nov 14 13:28:04 2026"
            ))
        );
        // From the Dock when the user has no loginwindow.
        assert_eq!(
            login_times(&table, 41001, 41000),
            Some(times(
                "Sat Nov 14 14:00:09 2026",
                "Sat Nov 14 14:00:00 2026"
            ))
        );
        for (pid, own) in [
            // Another user's daemon.
            (41001, 40686),
            // Not named `marsad`.
            (40686, 40686),
            (40900, 40686),
            (37805, 40686),
            // No such daemon, or no such asker.
            (77777, 40686),
            (5793, 77777),
            // A user without a GUI session.
            (41201, 41200),
        ] {
            assert_eq!(login_times(&table, pid, own), None, "{pid} {own}");
        }
    }

    // Only macOS reads the process table with `ps`.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn the_real_process_table_lists_this_process_and_a_marsad_child() {
        use std::os::unix::process::CommandExt as _;

        // A program that calls itself marsad, as `marsad` from a terminal does.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0("/x/marsad")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let is_marsad = |now: &Liveness| matches!(now, Liveness::Alive { exe, .. } if exe == Path::new("/x/marsad"));
        let Liveness::Alive { started, .. } =
            after_exec(async || identity(pid).await, is_marsad).await
        else {
            panic!("no identity for {pid}");
        };
        let table = process_table().await.expect("the table");
        let own = table.iter().find(|row| row.pid == std::process::id());
        let own = own.expect("this process is listed");
        let want = ProcessRow {
            pid,
            uid: own.uid,
            started,
            command: PathBuf::from("/x/marsad"),
        };
        assert!(table.contains(&want), "{want:?}");
        // Without a GUI session (a build machine may have none) it cannot be
        // told. With one, the child started in it.
        if let Some(times) = read_login_times(pid).await {
            assert_eq!(times.started, want.started);
            let order = crate::ps::started_order(times.login, times.started);
            assert_ne!(order, Some(std::cmp::Ordering::Greater), "{times:?}");
        }
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
