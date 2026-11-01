//! Optional system sleep inhibition while an agent is actively generating.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// The platform command that holds the machine awake for the current Voff process.
pub fn command(pid: u32) -> Option<Vec<String>> {
    if cfg!(target_os = "macos") {
        Some(vec![
            "caffeinate".into(),
            "-i".into(),
            "-w".into(),
            pid.to_string(),
        ])
    } else if cfg!(target_os = "linux") {
        Some(
            [
                "systemd-inhibit",
                "--what=idle:sleep",
                "--who=voff",
                "--why=agents are working",
                "--mode=block",
                "sleep",
                "infinity",
            ]
            .map(String::from)
            .to_vec(),
        )
    } else {
        None
    }
}

/// Holds a platform sleep inhibitor only while an agent is generating.
pub struct Awake {
    on: bool,
    program: Option<Vec<String>>,
    child: Option<Child>,
}

impl Awake {
    pub fn new(on: bool, program: Option<Vec<String>>) -> Self {
        Self {
            on,
            program,
            child: None,
        }
    }

    pub fn hold(&mut self, busy: bool) -> Result<(), String> {
        if let Some(child) = &mut self.child
            && let Ok(Some(status)) = child.try_wait()
        {
            self.child = None;
            let program = self.give_up();
            return Err(format!("{program} ended ({status})"));
        }
        if !(busy && self.on) {
            self.release();
            return Ok(());
        }
        if self.child.is_some() {
            return Ok(());
        }
        let Some(words) = &self.program else {
            return Ok(());
        };
        let mut command = Command::new(&words[0]);
        command
            .args(&words[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        match command.spawn() {
            Ok(child) => {
                self.child = Some(child);
                Ok(())
            }
            Err(error) => {
                let program = self.give_up();
                Err(format!("{program}: {error}"))
            }
        }
    }

    fn give_up(&mut self) -> String {
        self.program
            .take()
            .and_then(|words| words.into_iter().next())
            .unwrap_or_default()
    }

    fn release(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            if let Ok(pgid) = i32::try_from(child.id()) {
                let group = format!("-{pgid}");
                let stopped = Command::new("/bin/kill")
                    .args(["-TERM", "--", group.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                if !stopped {
                    let _ = child.kill();
                }
            }
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn holds_only_during_active_work_and_releases_on_drop() {
        let mut awake = Awake::new(true, Some(vec!["sleep".into(), "30".into()]));
        awake.hold(false).unwrap();
        assert!(awake.child.is_none());
        awake.hold(true).unwrap();
        let pid = awake.child.as_ref().unwrap().id();
        assert!(process_exists(pid));
        awake.hold(true).unwrap();
        assert_eq!(awake.child.as_ref().unwrap().id(), pid);
        awake.hold(false).unwrap();
        assert!(awake.child.is_none());
        assert!(!process_exists(pid));

        awake.hold(true).unwrap();
        let pid = awake.child.as_ref().unwrap().id();
        drop(awake);
        assert!(!process_exists(pid));
    }

    #[test]
    fn a_missing_inhibitor_is_reported_once() {
        let mut awake = Awake::new(true, Some(vec!["/missing/voff-inhibitor".into()]));
        assert!(
            awake
                .hold(true)
                .unwrap_err()
                .starts_with("/missing/voff-inhibitor:")
        );
        assert_eq!(awake.hold(true), Ok(()));
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        Command::new("/bin/kill")
            .args(["-0", "--", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}
