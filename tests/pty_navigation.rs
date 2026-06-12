use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[test]
fn dashboard_opens_pr_and_q_returns_home_before_exit() {
    let sandbox = TestSandbox::new();
    let binary = std::env::var("CARGO_BIN_EXE_tuicr").expect("CARGO_BIN_EXE_tuicr should be set");
    let mut session = PtySession::spawn(&binary, &sandbox);

    session.wait_for("Dashboard", Duration::from_secs(10));
    session.wait_for("Route test PR", Duration::from_secs(10));

    session.clear_buffer();
    session.send("\r");
    session.wait_for("src/lib.rs", Duration::from_secs(10));
    session.wait_for("Needs route attention", Duration::from_secs(10));

    session.clear_buffer();
    session.send("q");
    session.wait_for("Dashboard", Duration::from_secs(10));
    session.wait_for("Route test PR", Duration::from_secs(10));

    session.send("q");
    session.wait_for_exit(Duration::from_secs(10));

    let gh_log = fs::read_to_string(sandbox.gh_log()).expect("fake gh should have logged calls");
    assert!(
        gh_log.contains("search prs"),
        "dashboard did not call fake gh search: {gh_log}"
    );
    assert!(
        gh_log.contains("pr diff 1"),
        "PR open did not call fake gh diff: {gh_log}"
    );
    assert!(
        gh_log.contains("reviewThreads"),
        "PR open did not fetch mocked review threads: {gh_log}"
    );
}

struct TestSandbox {
    _temp: TempDir,
    cwd: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    gh_log: PathBuf,
}

impl TestSandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let cwd = temp.path().join("cwd");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&bin).expect("bin");
        let gh_log = temp.path().join("gh.log");
        write_fake_gh(&bin.join("gh"), &gh_log);
        Self {
            _temp: temp,
            cwd,
            home,
            bin,
            gh_log,
        }
    }

    fn gh_log(&self) -> &Path {
        &self.gh_log
    }

    fn path_env(&self) -> String {
        let original = std::env::var("PATH").unwrap_or_default();
        format!("{}:{original}", self.bin.display())
    }
}

struct PtySession {
    writer: Box<dyn Write + Send>,
    chunks: mpsc::Receiver<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    buffer: String,
}

impl PtySession {
    fn spawn(binary: &str, sandbox: &TestSandbox) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 140,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");

        let mut cmd = CommandBuilder::new(binary);
        cmd.cwd(&sandbox.cwd);
        cmd.env("PATH", sandbox.path_env());
        cmd.env("HOME", sandbox.home.to_string_lossy().to_string());
        cmd.env(
            "XDG_CONFIG_HOME",
            sandbox.home.join(".config").to_string_lossy().to_string(),
        );
        cmd.env(
            "TUICR_AGENT_STATE_DIR",
            sandbox
                .home
                .join(".tuicr-agent")
                .to_string_lossy()
                .to_string(),
        );

        let child = pair.slave.spawn_command(cmd).expect("spawn tuicr");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let writer = pair.master.take_writer().expect("pty writer");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            writer,
            chunks: rx,
            child,
            buffer: String::new(),
        }
    }

    fn send(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).expect("write keys");
        self.writer.flush().expect("flush keys");
    }

    fn clear_buffer(&mut self) {
        self.buffer.clear();
        while self.chunks.try_recv().is_ok() {}
    }

    fn wait_for(&mut self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.buffer.contains(needle) || visible_terminal_text(&self.buffer).contains(needle)
            {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self
                .chunks
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(chunk) => self.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        panic!(
            "timed out waiting for {needle:?}; visible screen buffer:\n{}\nraw buffer:\n{}",
            visible_terminal_text(&self.buffer),
            self.buffer
        );
    }

    fn wait_for_exit(mut self, timeout: Duration) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let status = self.child.wait();
            let _ = tx.send(status);
        });
        match rx.recv_timeout(timeout) {
            Ok(Ok(status)) => assert!(status.success(), "tuicr exited unsuccessfully: {status:?}"),
            Ok(Err(err)) => panic!("failed waiting for tuicr: {err}"),
            Err(_) => panic!("tuicr did not exit within {timeout:?}"),
        }
    }
}

fn visible_terminal_text(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch >= ' ' || ch == '\n' || ch == '\t' {
            output.push(ch);
        }
    }
    output
}

fn write_fake_gh(path: &Path, log_path: &Path) {
    let script = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "$*" >> {log}

if [[ "$*" == "api user --jq .login" ]]; then
  echo "tbedor"
  exit 0
fi

if [[ "$1 $2" == "search prs" ]]; then
  cat <<'JSON'
[
  {{
    "repository": {{ "nameWithOwner": "squareup/test" }},
    "number": 1,
    "title": "Route test PR",
    "url": "https://github.com/squareup/test/pull/1",
    "state": "open",
    "isDraft": false,
    "updatedAt": "2026-06-12T10:00:00Z"
  }}
]
JSON
  exit 0
fi

if [[ "$1 $2" == "pr view" ]]; then
  cat <<'JSON'
{{
  "number": 1,
  "title": "Route test PR",
  "url": "https://github.com/squareup/test/pull/1",
  "state": "OPEN",
  "isDraft": false,
  "author": {{ "login": "tbedor" }},
  "headRefName": "route-test",
  "baseRefName": "main",
  "headRefOid": "abcdef0123456789abcdef0123456789abcdef01",
  "baseRefOid": "1234567890abcdef1234567890abcdef12345678",
  "body": "",
  "updatedAt": "2026-06-12T10:00:00Z",
  "closed": false,
  "mergedAt": null,
  "statusCheckRollup": []
}}
JSON
  exit 0
fi

if [[ "$1 $2" == "pr diff" ]]; then
  cat <<'DIFF'
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
DIFF
  exit 0
fi

if [[ "$1" == "api" && "$*" == *"repos/squareup/test/pulls/1/commits"* ]]; then
  cat <<'JSON'
[
  {{
    "sha": "abcdef0123456789abcdef0123456789abcdef01",
    "commit": {{
      "message": "Change route test fixture",
      "author": {{
        "name": "T Bedor",
        "email": "tbedor@example.com",
        "date": "2026-06-12T10:00:00Z"
      }}
    }}
  }}
]
JSON
  exit 0
fi

if [[ "$1" == "api" && "$*" == *"repos/squareup/test/issues/1/comments"* ]]; then
  echo "[]"
  exit 0
fi

if [[ "$1" == "api" && "$2" == "graphql" && "$*" == *"reviewThreads"* ]]; then
  cat <<'JSON'
{{
  "data": {{
    "repository": {{
      "pullRequest": {{
        "reviewThreads": {{
          "pageInfo": {{ "hasNextPage": false, "endCursor": null }},
          "nodes": [
            {{
              "id": "PRRT_route",
              "isResolved": false,
              "isOutdated": false,
              "path": "src/lib.rs",
              "line": 1,
              "originalLine": 1,
              "diffSide": "RIGHT",
              "comments": {{
                "nodes": [
                  {{
                    "id": "PRRC_route_root",
                    "body": "Needs route attention",
                    "author": {{ "login": "reviewer" }},
                    "createdAt": "2026-06-12T10:01:00Z",
                    "url": "https://github.com/squareup/test/pull/1#discussion_r1"
                  }},
                  {{
                    "id": "PRRC_route_reply",
                    "body": "Follow-up reply is still unresolved",
                    "author": {{ "login": "tbedor" }},
                    "createdAt": "2026-06-12T10:02:00Z",
                    "url": "https://github.com/squareup/test/pull/1#discussion_r2",
                    "replyTo": {{ "id": "PRRC_route_root" }}
                  }}
                ]
              }}
            }}
          ]
        }}
      }}
    }}
  }}
}}
JSON
  exit 0
fi

if [[ "$1" == "api" && "$2" == "graphql" && "$*" == *"reviews"* ]]; then
  cat <<'JSON'
{{
  "data": {{
    "repository": {{
      "pullRequest": {{
        "reviews": {{
          "pageInfo": {{ "hasNextPage": false, "endCursor": null }},
          "nodes": []
        }}
      }}
    }}
  }}
}}
JSON
  exit 0
fi

echo "unexpected gh invocation: $*" >&2
exit 2
"#,
        log = shell_quote(log_path),
    );
    fs::write(path, script).expect("write fake gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).expect("fake gh metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("fake gh perms");
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
