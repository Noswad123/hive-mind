//! Bounded, thread-free pipe transport, independent of the macOS supervisor.
use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::Result;

pub(super) fn run(
    command: &mut Command,
    input: &[u8],
    deadline: Instant,
    cap: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        unix::run(command, input, deadline, cap, cancelled)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (command, input, deadline, cap, cancelled);
        anyhow::bail!("source processes require macOS or Linux")
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix {
    use super::*;
    use anyhow::{anyhow, bail, ensure};
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::{Child, Stdio},
        time::Duration,
    };

    struct Group(Child);
    impl Group {
        fn exited_successfully(&self) -> Result<Option<bool>> {
            // WNOWAIT pins the leader PID/PGID until Drop has finished signaling.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.0.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                bail!("source wait failed");
            }
            #[cfg(target_os = "linux")]
            let (pid, status) = unsafe { (info.si_pid(), info.si_status()) };
            #[cfg(target_os = "macos")]
            let (pid, status) = (info.si_pid, info.si_status);
            if pid == 0 {
                return Ok(None);
            }
            ensure!(pid == self.0.id() as libc::pid_t, "source wait failed");
            Ok(Some(info.si_code == libc::CLD_EXITED && status == 0))
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            // The leader is still unreaped, so its PID/PGID cannot be reused here.
            // Close descendants' pipes, then reap only our own child.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn nonblocking(fd: i32) -> Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "source pipe setup failed"
        );
        Ok(())
    }

    pub(super) fn run(
        command: &mut Command,
        input: &[u8],
        deadline: Instant,
        cap: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>> {
        ensure!(
            !cancelled.load(Ordering::Relaxed),
            "source operation cancelled"
        );
        ensure!(input.len() <= 1024 * 1024, "source input limit exceeded");
        ensure!(Instant::now() < deadline, "source process timed out");
        let deadline = deadline.min(Instant::now() + Duration::from_secs(30));
        let mut group = Group(
            command
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|_| anyhow!("source process could not start"))?,
        );
        let mut stdin = group.0.stdin.take();
        let mut stdout = group
            .0
            .stdout
            .take()
            .ok_or_else(|| anyhow!("source pipe unavailable"))?;
        let mut stderr = group
            .0
            .stderr
            .take()
            .ok_or_else(|| anyhow!("source pipe unavailable"))?;
        nonblocking(stdin.as_ref().unwrap().as_raw_fd())?;
        nonblocking(stdout.as_raw_fd())?;
        nonblocking(stderr.as_raw_fd())?;
        let mut written = 0;
        let mut total = 0usize;
        let mut output = Vec::new();
        let mut closed = [false; 2];
        let mut status = None;
        loop {
            ensure!(
                !cancelled.load(Ordering::Relaxed),
                "source operation cancelled"
            );
            ensure!(Instant::now() < deadline, "source process timed out");
            if written == input.len() {
                stdin.take();
            }
            if let Some(pipe) = stdin.as_mut() {
                match pipe.write(&input[written..]) {
                    Ok(0) => bail!("source input failed"),
                    Ok(n) => written += n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => bail!("source input failed"),
                }
            }
            for (i, pipe) in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read]
                .into_iter()
                .enumerate()
            {
                if closed[i] {
                    continue;
                }
                // One bounded read per pipe per turn prevents a noisy child starving the clock.
                let mut buf = [0; 16384];
                match pipe.read(&mut buf) {
                    Ok(0) => closed[i] = true,
                    Ok(n) => {
                        total += n;
                        ensure!(total <= cap, "source output limit exceeded");
                        if i == 0 {
                            output.extend_from_slice(&buf[..n]);
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => bail!("source output failed"),
                }
            }
            if status.is_none() {
                status = group.exited_successfully()?;
            }
            if let Some(status) = status {
                ensure!(status, "source process failed");
                if closed == [true, true] {
                    ensure!(written == input.len(), "source input incomplete");
                    return Ok(output);
                }
            }
            let mut fds = [
                libc::pollfd {
                    fd: if closed[0] { -1 } else { stdout.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if closed[1] { -1 } else { stderr.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: stdin.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLOUT,
                    revents: 0,
                },
            ];
            unsafe {
                libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 2);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn observe_exit(group: &Group) -> bool {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(success) = group.exited_successfully().unwrap() {
                    return success;
                }
                assert!(Instant::now() < deadline, "fixture child did not exit");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        #[test]
        fn exit_observation_preserves_waitable_leader_until_group_cleanup() {
            for (script, expected) in [
                ("exit 0", true),
                ("exit 7", false),
                ("kill -TERM $$", false),
            ] {
                let group = Group(
                    Command::new("/bin/sh")
                        .args(["-c", script])
                        .env_clear()
                        .process_group(0)
                        .spawn()
                        .unwrap(),
                );
                assert_eq!(observe_exit(&group), expected);
                // Repeated WNOWAIT must still find the same waitable exit, not ECHILD.
                assert_eq!(group.exited_successfully().unwrap(), Some(expected));
                assert_eq!(group.exited_successfully().unwrap(), Some(expected));
                drop(group);
            }
        }

        #[test]
        fn exited_leader_stays_waitable_while_descendant_holds_pipe_until_cleanup() {
            let mut group = Group(
                Command::new("/bin/sh")
                    .args(["-c", "/bin/sleep 10 & printf ready; exit 0"])
                    .env_clear()
                    .process_group(0)
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let mut stdout = group.0.stdout.take().unwrap();
            nonblocking(stdout.as_raw_fd()).unwrap();
            assert!(observe_exit(&group));
            let mut bytes = Vec::new();
            assert_eq!(
                stdout.read_to_end(&mut bytes).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            assert_eq!(bytes, b"ready");
            assert_eq!(group.exited_successfully().unwrap(), Some(true));
            drop(group);
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match stdout.read_to_end(&mut bytes) {
                    Ok(_) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "descendant retained pipe after cleanup"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("fixture pipe failed: {e}"),
                }
            }
            assert_eq!(bytes, b"ready");
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timeout_output_stderr_and_exit_are_bounded_and_redacted() {
        for (script, cap, expected) in [
            ("sleep 10 & wait", 1024, "timed out"),
            ("while :; do printf secret; done", 1024, "output limit"),
            ("while :; do printf secret >&2; done", 1024, "output limit"),
            ("printf secret >&2; exit 1", 1024, "process failed"),
            ("sleep 10 & exit 0", 1024, "timed out"),
        ] {
            let start = Instant::now();
            let error = run(
                Command::new("/bin/sh").args(["-c", script]).env_clear(),
                b"",
                start + Duration::from_millis(200),
                cap,
                &AtomicBool::new(false),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret"));
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn simultaneous_input_and_output_are_byte_exact() {
        let input = vec![b'x'; 512 * 1024];
        let out = run(
            Command::new("/bin/cat").env_clear(),
            &input,
            Instant::now() + Duration::from_secs(5),
            input.len(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn expired_deadline_input_limit_and_spawn_failure_are_static_errors() {
        let mut command = Command::new("/does/not/exist");
        let error = run(
            &mut command,
            b"",
            Instant::now(),
            1024,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "source process timed out");
        let error = run(
            &mut command,
            &vec![0; 1024 * 1024 + 1],
            Instant::now() + Duration::from_secs(1),
            1024,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "source input limit exceeded");
        let error = run(
            &mut command,
            b"",
            Instant::now() + Duration::from_secs(1),
            1024,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "source process could not start");
    }
}
