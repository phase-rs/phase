//! Boot-time audio-device health probe.
//!
//! WebKitGTK's WebProcess opens the OS audio device *synchronously on the page
//! main thread* the first time the page constructs an `AudioContext`. When the
//! PipeWire data plane is wedged (streams hang while the control plane still
//! answers — e.g. the uaccess-ACL boot race, pipewire#423/#2534), that open
//! never returns and the entire page freezes: timers dead, first frame stale.
//! The page cannot defend itself — JS has no way to time out a synchronous
//! block — so the shell probes the device from a killable child process and
//! the frontend skips audio entirely on a `Wedged` verdict.
//!
//! Instrument choice: the primary probe plays through GStreamer's
//! `autoaudiosink`, which resolves to the same sink element (`pulsesink`, via
//! libpulse to pipewire-pulse) that WebKitGTK's own GStreamer stack loads —
//! the probe traverses the exact protocol path whose hang we are predicting.
//! Fallbacks cover systems without `gst-launch-1.0` on PATH. A tool that
//! exits quickly with an error proves nothing about hangs, so it falls
//! through to the next tool; only a clean exit is `Healthy` and only a
//! timeout is `Wedged`. An exhausted ladder is `Unknown`, which the frontend
//! treats as healthy: fail-open, preserving today's behavior on systems where
//! no probe tool exists.
//!
//! Side effects, accepted and deliberate: the probe opens a real (silent)
//! playback stream each launch, which resumes a suspended sink; and if the
//! app quits during the probe window (≤3 s) the child is orphaned rather
//! than torn down — it is a single tiny process that exits or is reaped by
//! init, not worth a teardown path.

use serde::Serialize;
#[cfg(any(target_os = "linux", all(test, unix)))]
use std::process::{Command, ExitStatus, Stdio};
#[cfg(any(target_os = "linux", all(test, unix)))]
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBootHealth {
    Healthy,
    // This serialized verdict remains part of the command contract on mobile,
    // even though only Linux can produce it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Wedged,
    Unknown,
}

#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_millis(3000);

static VERDICT: OnceCell<AudioBootHealth> = OnceCell::const_new();

async fn resolve_verdict() -> AudioBootHealth {
    *VERDICT
        .get_or_init(|| async {
            tauri::async_runtime::spawn_blocking(probe_system)
                .await
                .unwrap_or(AudioBootHealth::Unknown)
        })
        .await
}

/// Start resolving the verdict in the background so the frontend's
/// `audio_boot_health` invoke usually finds it already cached.
#[cfg(desktop)]
pub fn prewarm() {
    tauri::async_runtime::spawn(async {
        let _ = resolve_verdict().await;
    });
}

#[tauri::command]
pub async fn audio_boot_health() -> AudioBootHealth {
    resolve_verdict().await
}

/// Only Linux/WebKitGTK has the synchronous device-open failure mode;
/// WebView2 and WKWebView keep audio out of the page main thread.
#[cfg(not(target_os = "linux"))]
fn probe_system() -> AudioBootHealth {
    AudioBootHealth::Healthy
}

#[cfg(target_os = "linux")]
fn probe_system() -> AudioBootHealth {
    let wav = write_silence_wav();
    let mut ladder: Vec<Command> = Vec::new();

    let mut gst = Command::new("gst-launch-1.0");
    gst.args([
        "-q",
        "audiotestsrc",
        "num-buffers=8",
        "volume=0.0",
        "!",
        "autoaudiosink",
    ]);
    ladder.push(gst);

    if let Some(wav) = &wav {
        let mut paplay = Command::new("paplay");
        paplay.arg(wav);
        ladder.push(paplay);
        let mut pw_play = Command::new("pw-play");
        pw_play.args(["--volume", "0"]).arg(wav);
        ladder.push(pw_play);
    }

    let verdict = run_probe_ladder(ladder, PROBE_TIMEOUT);
    if let Some(wav) = wav {
        let _ = std::fs::remove_file(wav);
    }
    verdict
}

/// ~10 ms of s16 mono silence for the file-consuming fallback tools.
#[cfg(target_os = "linux")]
fn write_silence_wav() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("phase-audio-probe-{}.wav", std::process::id()));
    let data_len: u32 = 960;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&48_000u32.to_le_bytes()); // sample rate
    bytes.extend_from_slice(&96_000u32.to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    // create_new: never follow a pre-planted symlink in the shared temp dir.
    // Unlink any stale leftover first (crashed run + recycled PID) so
    // create_new doesn't spuriously fail; create_new itself never follows.
    let _ = std::fs::remove_file(&path);
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    file.write_all(&bytes).ok()?;
    Some(path)
}

#[cfg(any(target_os = "linux", all(test, unix)))]
enum ProbeOutcome {
    Passed,
    FailedFast,
    SpawnFailed,
    /// Child exceeded the timeout; carries the post-kill wait status so the
    /// tests can prove the child was actually reaped (production only needs
    /// the discriminant, hence the test-gated read).
    TimedOut(#[cfg_attr(not(test), allow(dead_code))] ExitStatus),
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn run_single_probe(cmd: &mut Command, timeout: Duration) -> ProbeOutcome {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return ProbeOutcome::SpawnFailed,
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    ProbeOutcome::Passed
                } else {
                    ProbeOutcome::FailedFast
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    // Blocks until the child is really gone — reaps the
                    // zombie and, in tests, proves kill() took effect.
                    return match child.wait() {
                        Ok(status) => ProbeOutcome::TimedOut(status),
                        Err(_) => ProbeOutcome::FailedFast,
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return ProbeOutcome::FailedFast,
        }
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn run_probe_ladder(ladder: Vec<Command>, timeout: Duration) -> AudioBootHealth {
    for mut cmd in ladder {
        match run_single_probe(&mut cmd, timeout) {
            ProbeOutcome::Passed => return AudioBootHealth::Healthy,
            // A hanging stream-open IS the phenomenon, whichever tool hit it.
            ProbeOutcome::TimedOut(_) => return AudioBootHealth::Wedged,
            // Fast failure or missing tool proves nothing about hangs.
            ProbeOutcome::FailedFast | ProbeOutcome::SpawnFailed => continue,
        }
    }
    AudioBootHealth::Unknown
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn cmd(program: &str, args: &[&str]) -> Command {
        let mut c = Command::new(program);
        c.args(args);
        c
    }

    #[test]
    fn ladder_reaches_healthy_via_a_later_tool() {
        let ladder = vec![
            cmd("phase-no-such-probe-tool", &[]),
            cmd("false", &[]),
            cmd("true", &[]),
        ];
        assert_eq!(
            run_probe_ladder(ladder, Duration::from_secs(5)),
            AudioBootHealth::Healthy
        );
    }

    #[test]
    fn exhausted_ladder_is_unknown_not_wedged() {
        let ladder = vec![cmd("phase-no-such-probe-tool", &[]), cmd("false", &[])];
        assert_eq!(
            run_probe_ladder(ladder, Duration::from_secs(5)),
            AudioBootHealth::Unknown
        );
    }

    #[test]
    fn hang_is_wedged_and_the_child_is_really_killed() {
        let start = Instant::now();
        let outcome = run_single_probe(&mut cmd("sleep", &["30"]), Duration::from_millis(300));
        // wait() after kill() blocks until the child dies: if kill() were a
        // no-op this would take the full 30 s and the elapsed bound fails.
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome {
            ProbeOutcome::TimedOut(status) => {
                assert_eq!(
                    status.signal(),
                    Some(libc_sigkill()),
                    "child must die by SIGKILL"
                );
            }
            _ => panic!("a hanging probe must report TimedOut"),
        }
        assert_eq!(
            run_probe_ladder(vec![cmd("sleep", &["30"])], Duration::from_millis(300)),
            AudioBootHealth::Wedged
        );
    }

    // Avoid a libc dependency for one constant.
    fn libc_sigkill() -> i32 {
        9
    }

    #[test]
    fn silent_wav_is_parseable_riff() {
        #[cfg(target_os = "linux")]
        {
            let path = write_silence_wav().expect("wav written");
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
            assert_eq!(bytes.len(), 44 + 960);
            assert!(
                bytes[44..].iter().all(|&b| b == 0),
                "payload must be silence"
            );
            std::fs::remove_file(path).unwrap();
        }
    }
}
