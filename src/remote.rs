//! Remote mode: watch another Linux machine (a cloud server, a home server,
//! a Raspberry Pi) over SSH.
//!
//! The window runs `ssh <server> sysmon --agent`. The agent takes readings
//! exactly as the window would locally and writes one JSON message per line
//! to its output; nothing listens on the network and no port is opened.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, StreamExt};
use iced::Subscription;
use serde::{Deserialize, Serialize};

use crate::report::{self, Section};
use crate::stats::{uname_machine, Collector, Snapshot, StaticInfo};

/// Bumped when the messages change in a way older agents cannot follow.
pub const PROTOCOL: u32 = 1;
const RELEASES: &str = "https://github.com/BlissNathan1305/sysmon/releases/download";

/// One line of agent output.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessage {
    Hello { protocol: u32, version: String, uid: u32, info: StaticInfo },
    Snapshot(Box<Snapshot>),
    Report(Vec<Section>),
}

// ---- the agent (runs on the server) ----

/// Takes readings until whoever reads our output goes away. Returns the
/// process exit code.
pub fn run_agent(interval: Duration) -> i32 {
    let info = StaticInfo::load();
    let mut collector = Collector::new(info.cpu_threads);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut send = |m: &AgentMessage| -> bool {
        serde_json::to_writer(&mut out, m).is_ok() && out.write_all(b"\n").is_ok() && out.flush().is_ok()
    };
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let hello =
        AgentMessage::Hello { protocol: PROTOCOL, version: env!("CARGO_PKG_VERSION").into(), uid, info: info.clone() };
    if !send(&hello) {
        return 1;
    }
    // The first reading only sets the baseline that rates are measured from.
    collector.sample();
    let mut last_report: Option<Instant> = None;
    loop {
        std::thread::sleep(interval);
        let snap = collector.sample();
        if last_report.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) {
            last_report = Some(Instant::now());
            if !send(&AgentMessage::Report(report::gather(&info, &snap))) {
                return 0;
            }
        }
        // A failed write means the viewer disconnected: stop quietly.
        if !send(&AgentMessage::Snapshot(Box::new(snap))) {
            return 0;
        }
    }
}

// ---- saved servers ----

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Server {
    pub name: String,
    /// What you would type after `ssh`: `user@host`, a host, or a ~/.ssh/config alias.
    pub target: String,
    pub port: Option<u16>,
    /// A private key file such as an AWS `.pem`.
    pub identity: Option<String>,
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

impl Server {
    /// Checks what was typed into the form and builds a server from it.
    pub fn new(name: &str, target: &str, port: &str, identity: &str) -> Result<Server, String> {
        let target = clean(target);
        if target.is_empty() {
            return Err("Enter the server's address, for example ubuntu@203.0.113.10.".into());
        }
        // A leading dash would be read by ssh as an option.
        if target.starts_with('-') || target.contains(char::is_whitespace) {
            return Err("The address cannot start with '-' or contain spaces.".into());
        }
        let port = match port.trim() {
            "" => None,
            p => Some(p.parse::<u16>().ok().filter(|p| *p > 0).ok_or("The port must be a number from 1 to 65535.")?),
        };
        let identity = Some(clean(identity)).filter(|i| !i.is_empty());
        if let Some(path) = &identity {
            if !expand_home(path).is_file() {
                return Err(format!("The key file {path} was not found."));
            }
        }
        let name = Some(clean(name)).filter(|n| !n.is_empty()).unwrap_or_else(|| target.clone());
        Ok(Server { name, target, port, identity })
    }

    /// One settings-file line: fields separated by tabs.
    pub fn to_pref(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.name,
            self.target,
            self.port.map(|p| p.to_string()).unwrap_or_default(),
            self.identity.clone().unwrap_or_default()
        )
    }

    pub fn from_pref(line: &str) -> Option<Server> {
        let f: Vec<&str> = line.split('\t').collect();
        let (name, target) = (f.first()?.trim(), f.get(1)?.trim());
        if name.is_empty() || target.is_empty() || target.starts_with('-') {
            return None;
        }
        Some(Server {
            name: name.into(),
            target: target.into(),
            port: f.get(2).and_then(|p| p.trim().parse().ok()),
            identity: f.get(3).map(|i| i.trim().to_string()).filter(|i| !i.is_empty()),
        })
    }

    /// Arguments for `ssh`, ending with the destination.
    fn ssh_args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "-T",
            // Readings are repetitive text; compression shrinks them several times.
            "-C",
            // Never stop to ask for a password: there is no terminal to type it in.
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=12",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            // Trust a server the first time, like typing "yes" at the prompt;
            // a changed key is still refused.
            "-o",
            "StrictHostKeyChecking=accept-new",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if let Some(p) = self.port {
            a.extend(["-p".into(), p.to_string()]);
        }
        if let Some(i) = &self.identity {
            a.extend([
                "-i".into(),
                expand_home(i).to_string_lossy().into_owned(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
            ]);
        }
        a.push(self.target.clone());
        a
    }
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// The remote command: find sysmon (preferring the copy "Install on server"
/// puts in ~/.local/bin) and start the agent. Run through `sh` because the
/// login shell may be fish or csh.
fn agent_command(interval_ms: u64) -> String {
    format!(
        "sh -c 'P=\"$HOME/.local/bin/sysmon\"; [ -x \"$P\" ] || P=$(command -v sysmon 2>/dev/null); \
         if [ -z \"$P\" ] || [ ! -x \"$P\" ]; then echo SYSMON_NOT_INSTALLED; exit 3; fi; \
         exec \"$P\" --agent --interval {interval_ms}'"
    )
}

// ---- the connection (runs on this computer) ----

#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    NotInstalled,
    /// The server has a sysmon too old (or too new) to talk to this one.
    Outdated(String),
    Other(String),
}

#[derive(Debug, Clone)]
pub enum Event {
    Hello { info: StaticInfo, uid: u32, version: String },
    Snapshot(Arc<Snapshot>),
    Report(Vec<Section>),
    Failed(Failure),
}

/// Ends the ssh process when the connection is dropped (switching away,
/// changing the interval, or closing the window).
struct KillOnDrop(Arc<Mutex<Child>>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Ok(mut c) = self.0.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A live connection to `server`. A new `attempt` number starts a fresh one.
pub fn subscription(server: Server, interval_ms: u64, attempt: u32) -> Subscription<Event> {
    let id = (server.clone(), interval_ms, attempt);
    iced::subscription::channel(id, 64, move |mut output| async move {
        let (tx, mut rx) = mpsc::unbounded::<Event>();
        let spawned = Command::new("ssh")
            .args(server.ssh_args())
            .arg(agent_command(interval_ms))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let _guard = match spawned {
            Ok(mut child) => {
                let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
                std::thread::spawn(move || read_agent(stdout, stderr, tx));
                Some(KillOnDrop(Arc::new(Mutex::new(child))))
            }
            Err(e) => {
                let msg = if e.kind() == std::io::ErrorKind::NotFound {
                    "The ssh program is not installed on this computer (package openssh-client).".to_string()
                } else {
                    format!("Could not start ssh: {e}")
                };
                let _ = output.send(Event::Failed(Failure::Other(msg))).await;
                None
            }
        };
        while let Some(event) = rx.next().await {
            if output.send(event).await.is_err() {
                break;
            }
        }
        // Stay alive (and keep the guard) until the window drops us.
        loop {
            std::future::pending::<()>().await;
        }
    })
}

fn read_agent(
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    tx: mpsc::UnboundedSender<Event>,
) {
    let err_text = Arc::new(Mutex::new(String::new()));
    let err_thread = stderr.map(|mut e| {
        let buf = err_text.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            if let Ok(mut b) = buf.lock() {
                *b = s;
            }
        })
    });
    let mut connected = false;
    if let Some(out) = stdout {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if line.trim() == "SYSMON_NOT_INSTALLED" {
                let _ = tx.unbounded_send(Event::Failed(Failure::NotInstalled));
                return;
            }
            let event = match serde_json::from_str::<AgentMessage>(&line) {
                Ok(AgentMessage::Hello { protocol, version, uid, info }) => {
                    if protocol != PROTOCOL {
                        let _ = tx.unbounded_send(Event::Failed(Failure::Outdated(version)));
                        return;
                    }
                    connected = true;
                    Event::Hello { info, uid, version }
                }
                Ok(AgentMessage::Snapshot(s)) => Event::Snapshot(Arc::from(s)),
                Ok(AgentMessage::Report(r)) => Event::Report(r),
                // Login banners or shell noise; not ours.
                Err(_) => continue,
            };
            if tx.unbounded_send(event).is_err() {
                return;
            }
        }
    }
    if let Some(t) = err_thread {
        let _ = t.join();
    }
    let stderr = err_text.lock().map(|s| s.clone()).unwrap_or_default();
    // sysmon before 0.4 answers "unknown option '--agent'".
    let failure = if stderr.contains("unknown option") && stderr.contains("--agent") {
        Failure::Outdated("an older version".into())
    } else {
        Failure::Other(explain(&stderr, connected))
    };
    let _ = tx.unbounded_send(Event::Failed(failure));
}

/// Turns ssh's error output into advice a person can act on.
pub fn explain(stderr: &str, was_connected: bool) -> String {
    let has = |s: &str| stderr.contains(s);
    if has("Permission denied") {
        "The server refused the login. Check the user name (for example ubuntu@ or ec2-user@) and the key file. Password logins are not supported.".into()
    } else if has("Could not resolve hostname") {
        "The server name could not be found. Check the address.".into()
    } else if has("Connection refused") {
        "The server is not accepting SSH connections on that port.".into()
    } else if has("timed out") || has("No route to host") || has("Network is unreachable") {
        "The server could not be reached. Check the address, and that its firewall or security group allows SSH (port 22) from this computer.".into()
    } else if has("REMOTE HOST IDENTIFICATION HAS CHANGED") || has("Host key verification failed") {
        "The server's identity is different from last time, so the connection was stopped for safety. If the server was rebuilt, remove the old key with: ssh-keygen -R <address>".into()
    } else if has("UNPROTECTED PRIVATE KEY") || has("bad permissions") {
        "The key file can be read by other users, so ssh will not use it. Fix it with: chmod 600 <key file>".into()
    } else if has("not accessible") || has("No such file") {
        "The key file could not be read.".into()
    } else if was_connected {
        "The connection to the server was lost.".into()
    } else {
        stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("Warning: Permanently added"))
            .map(|l| format!("The connection failed: {l}"))
            .unwrap_or_else(|| "The connection closed before the server answered.".into())
    }
}

// ---- installing the agent on a server ----

/// Runs one ssh command with optional input, giving up after `limit`.
fn ssh_run(server: &Server, command: &str, input: Option<Vec<u8>>, limit: Duration) -> Result<String, String> {
    let mut child = Command::new("ssh")
        .args(server.ssh_args())
        .arg(command)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start ssh: {e}"))?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The server took too long to answer.".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = (out_t.join().unwrap_or_default(), err_t.join().unwrap_or_default());
    if status.success() {
        Ok(out)
    } else {
        Err(if out.contains("NO_DOWNLOADER") {
            "The server has neither curl nor wget to download sysmon.".into()
        } else if err.contains("404") || err.contains("Not Found") {
            "No ready-made download matches this server.".into()
        } else {
            explain(&err, false)
        })
    }
}

/// Puts this version of sysmon in ~/.local/bin on the server: downloaded
/// there from the GitHub release, or, if that fails and the processors
/// match, copied from this computer (`program` is this sysmon's file).
/// Returns the installed version line.
pub fn install(server: &Server, program: &std::path::Path) -> Result<String, String> {
    let v = env!("CARGO_PKG_VERSION");
    let download = format!(
        "sh -c 'set -e; A=$(uname -m); U={RELEASES}/v{v}/sysmon-{v}-$A-linux.tar.gz; T=$(mktemp -d); \
         trap \"rm -rf $T\" EXIT; \
         if command -v curl >/dev/null 2>&1; then curl -fsSL \"$U\" -o \"$T/s.tgz\"; \
         elif command -v wget >/dev/null 2>&1; then wget -qO \"$T/s.tgz\" \"$U\"; \
         else echo NO_DOWNLOADER; exit 4; fi; \
         tar -xzf \"$T/s.tgz\" -C \"$T\"; mkdir -p \"$HOME/.local/bin\"; \
         install -m 755 \"$T\"/sysmon-*/sysmon \"$HOME/.local/bin/sysmon\"; \"$HOME/.local/bin/sysmon\" --version'"
    );
    let first = match ssh_run(server, &download, None, Duration::from_secs(180)) {
        Ok(out) => return Ok(out.trim().to_string()),
        Err(e) => e,
    };
    // Copy this computer's program instead, if it can run there.
    let remote_arch = ssh_run(server, "uname -m", None, Duration::from_secs(30)).map(|a| a.trim().to_string())?;
    if remote_arch != uname_machine() {
        return Err(format!("{first} The server's processor ({remote_arch}) differs from this computer's, so this copy cannot be used either."));
    }
    let exe = std::fs::read(program).map_err(|e| format!("Could not read this program: {e}"))?;
    let upload =
        "sh -c 'mkdir -p \"$HOME/.local/bin\"; N=\"$HOME/.local/bin/sysmon.new\"; cat > \"$N\"; chmod 755 \"$N\"; \
                  if \"$N\" --version; then mv \"$N\" \"$HOME/.local/bin/sysmon\"; else rm -f \"$N\"; exit 5; fi'";
    ssh_run(server, upload, Some(exe), Duration::from_secs(300)).map(|out| out.trim().to_string()).map_err(|_| {
        format!("{first} This computer's copy of sysmon does not run on the server (its system is older). Install sysmon there from the releases page.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_form_is_checked() {
        let s = Server::new("", "ubuntu@203.0.113.10", "", "").unwrap();
        assert_eq!(s.name, "ubuntu@203.0.113.10");
        assert_eq!(s.port, None);
        assert!(Server::new("x", "-oProxyCommand=evil", "", "").is_err(), "options must not be injectable");
        assert!(Server::new("x", "host name", "", "").is_err());
        assert!(Server::new("x", "host", "99999", "").is_err());
        assert!(Server::new("x", "host", "", "/no/such/key.pem").is_err());
        assert_eq!(Server::new("web", " root@web ", "2222", "").unwrap().port, Some(2222));
    }

    #[test]
    fn servers_round_trip_through_settings() {
        let s = Server {
            name: "Web 1".into(),
            target: "ec2-user@web1".into(),
            port: Some(22),
            identity: Some("~/.ssh/aws.pem".into()),
        };
        assert_eq!(Server::from_pref(&s.to_pref()), Some(s));
        assert_eq!(Server::from_pref("bad\t-oX"), None);
    }

    #[test]
    fn ssh_arguments_end_with_the_target() {
        let s = Server { name: "n".into(), target: "pi@raspberrypi".into(), port: Some(2200), identity: None };
        let a = s.ssh_args();
        assert_eq!(a.last().unwrap(), "pi@raspberrypi");
        assert!(a.windows(2).any(|w| w == ["-p", "2200"]));
        assert!(a.iter().any(|x| x == "BatchMode=yes"));
    }

    #[test]
    fn agent_messages_round_trip() {
        let snap = Snapshot { cpu_total: 42.5, uptime: 99, ..Default::default() };
        let line = serde_json::to_string(&AgentMessage::Snapshot(Box::new(snap))).unwrap();
        match serde_json::from_str::<AgentMessage>(&line).unwrap() {
            AgentMessage::Snapshot(s) => assert_eq!((s.cpu_total, s.uptime), (42.5, 99)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ssh_errors_become_advice() {
        assert!(explain("ubuntu@1.2.3.4: Permission denied (publickey).", false).contains("refused the login"));
        assert!(explain("ssh: connect to host 1.2.3.4 port 22: Connection timed out", false).contains("security group"));
        assert!(
            explain("ssh: Could not resolve hostname nope: Name or service not known", false).contains("not be found")
        );
        assert_eq!(explain("", true), "The connection to the server was lost.");
    }

    /// Talks to a real SSH server. Run with, for example:
    /// SYSMON_TEST_SSH=cloud@127.0.0.1 SYSMON_TEST_PORT=2222 SYSMON_TEST_KEY=/path/key cargo test -- --ignored live
    #[test]
    #[ignore]
    fn live_server_install_and_connect() {
        use iced::futures::executor::block_on;
        let target = std::env::var("SYSMON_TEST_SSH").expect("SYSMON_TEST_SSH");
        let server = Server::new(
            "test",
            &target,
            &std::env::var("SYSMON_TEST_PORT").unwrap_or_default(),
            &std::env::var("SYSMON_TEST_KEY").unwrap_or_default(),
        )
        .unwrap();
        let connect = |server: &Server| -> Vec<Event> {
            let mut child = Command::new("ssh")
                .args(server.ssh_args())
                .arg(agent_command(500))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let (tx, mut rx) = mpsc::unbounded();
            let (out, err) = (child.stdout.take(), child.stderr.take());
            std::thread::spawn(move || read_agent(out, err, tx));
            let mut events = Vec::new();
            while let Some(e) = block_on(rx.next()) {
                let done = matches!(e, Event::Snapshot(_) | Event::Failed(_));
                events.push(e);
                if done {
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            events
        };
        let clear = "rm -f \"$HOME/.local/bin/sysmon\"";
        ssh_run(&server, &format!("sh -c '{clear}'"), None, Duration::from_secs(30)).unwrap();

        let events = connect(&server);
        assert!(matches!(events.last(), Some(Event::Failed(Failure::NotInstalled))), "{events:?}");

        // A copy too old to know --agent.
        let fake = "mkdir -p \"$HOME/.local/bin\"; printf \"#!/bin/sh\\necho \\\"sysmon: unknown option --agent\\\" >&2; echo \\\"sysmon: unknown option \\x27--agent\\x27\\\" >&2; exit 2\\n\" > \"$HOME/.local/bin/sysmon\"; chmod 755 \"$HOME/.local/bin/sysmon\"";
        ssh_run(&server, &format!("sh -c '{fake}'"), None, Duration::from_secs(30)).unwrap();
        let events = connect(&server);
        assert!(matches!(events.last(), Some(Event::Failed(Failure::Outdated(_)))), "{events:?}");

        let program = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/target/release/sysmon"));
        let installed = install(&server, program).expect("install");
        assert!(installed.contains(env!("CARGO_PKG_VERSION")), "{installed}");

        let events = connect(&server);
        assert!(matches!(events.first(), Some(Event::Hello { .. })), "{events:?}");
        match events.last() {
            Some(Event::Snapshot(s)) => assert!(s.mem.total > 0 && !s.procs.is_empty()),
            other => panic!("expected a reading, got {other:?}"),
        }
    }

    #[test]
    fn agent_command_has_no_single_quotes_inside() {
        let c = agent_command(1000);
        assert!(c.starts_with("sh -c '") && c.ends_with('\''));
        assert_eq!(c.matches('\'').count(), 2);
    }
}
