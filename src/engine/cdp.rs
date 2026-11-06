use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// DevTools client speaking the `--remote-debugging-pipe` transport: JSON
/// messages terminated by a NUL byte, commands on fd 3, replies on fd 4.
///
/// A reader thread owns the read end and forwards every message into a channel
/// so unsolicited events (screencast frames in particular) are not dropped
/// while we are blocked waiting for a reply.
pub struct Cdp {
    child: Child,
    w: File,
    rx: Receiver<Value>,
    /// events seen while waiting for a reply
    pending: VecDeque<Value>,
    next: u64,
    pub pid: u32,
    dead: bool,
}

impl Cdp {
    pub fn spawn(exe: &Path, args: &[String], log: Option<File>) -> Result<Cdp> {
        let mut cmd_pipe = [0i32; 2];
        let mut resp_pipe = [0i32; 2];
        unsafe {
            if libc::pipe(cmd_pipe.as_mut_ptr()) != 0 {
                return Err(anyhow!("pipe: {}", std::io::Error::last_os_error()));
            }
            if libc::pipe(resp_pipe.as_mut_ptr()) != 0 {
                return Err(anyhow!("pipe: {}", std::io::Error::last_os_error()));
            }
        }
        let (cmd_read, cmd_write) = (cmd_pipe[0], cmd_pipe[1]);
        let (resp_read, resp_write) = (resp_pipe[0], resp_pipe[1]);

        // Chromium expects the pipes on fd 3 and fd 4. If `pipe()` happened to
        // hand us 3 or 4 (which is exactly what a plain shell gives us), the
        // dup2 below would clobber the descriptor we are about to duplicate and
        // the closing loop would then close our freshly installed pipes —
        // Chromium then dies with "Remote debugging pipe file descriptors are
        // not open". Move the ends we pass down to high descriptors first, so 3
        // and 4 are guaranteed to be free.
        let cmd_src = unsafe { libc::fcntl(cmd_read, libc::F_DUPFD, 10) };
        let resp_src = unsafe { libc::fcntl(resp_write, libc::F_DUPFD, 10) };
        if cmd_src < 0 || resp_src < 0 {
            let e = std::io::Error::last_os_error();
            unsafe {
                libc::close(cmd_read);
                libc::close(cmd_write);
                libc::close(resp_read);
                libc::close(resp_write);
                if cmd_src >= 0 {
                    libc::close(cmd_src);
                }
                if resp_src >= 0 {
                    libc::close(resp_src);
                }
            }
            return Err(anyhow!("fcntl(F_DUPFD): {e}"));
        }
        unsafe {
            libc::close(cmd_read);
            libc::close(resp_write);
        }

        let mut cmd = Command::new(exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(match log {
                Some(f) => Stdio::from(f),
                None => Stdio::null(),
            });

        unsafe {
            cmd.pre_exec(move || {
                // own process group: Chromium spawns renderer/gpu/audio children
                // and killing only the browser can leave them behind, which
                // then keeps playing audio from a "closed" tab.
                libc::setpgid(0, 0);
                if libc::dup2(cmd_src, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::dup2(resp_src, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // never close 3 or 4: one of these originals may *be* 3/4 and
                // has just been replaced by the dup2 above
                for fd in [cmd_src, resp_src, cmd_write, resp_read] {
                    if fd > 4 {
                        libc::close(fd);
                    }
                }
                Ok(())
            });
        }

        let child = cmd.spawn()?;
        unsafe {
            libc::close(cmd_src);
            libc::close(resp_src);
        }
        let w = unsafe { File::from_raw_fd(cmd_write) };
        let r = unsafe { File::from_raw_fd(resp_read) };
        let pid = child.id();

        let (tx, rx) = channel::<Value>();
        std::thread::spawn(move || {
            let mut r = r;
            let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
            let mut chunk = vec![0u8; 1 << 16];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(pos) = buf.iter().position(|&b| b == 0) {
                            let msg: Vec<u8> = buf.drain(..=pos).collect();
                            if let Ok(v) = serde_json::from_slice::<Value>(&msg[..msg.len() - 1]) {
                                if tx.send(v).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(Cdp {
            child,
            w,
            rx,
            pending: VecDeque::new(),
            next: 0,
            pid,
            dead: false,
        })
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    pub fn call(
        &mut self,
        method: &str,
        params: Option<Value>,
        session: Option<&str>,
    ) -> Result<Value> {
        if self.dead {
            return Err(anyhow!("browser closed the debug pipe"));
        }
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let mut data = serde_json::to_vec(&msg)?;
        data.push(0);
        if let Err(e) = self.w.write_all(&data).and_then(|_| self.w.flush()) {
            self.dead = true;
            return Err(anyhow!("browser closed the debug pipe ({e})"));
        }

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(anyhow!("timed out waiting for {method}"));
            }
            let v = match self.rx.recv_timeout(deadline - now) {
                Ok(v) => v,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(anyhow!("timed out waiting for {method}"))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.dead = true;
                    return Err(anyhow!("browser closed the debug pipe"));
                }
            };
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("{method}: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            self.pending.push_back(v);
            if self.pending.len() > 4096 {
                self.pending.pop_front();
            }
        }
    }

    /// Everything the browser pushed at us since the last call.
    pub fn drain_events(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = self.pending.drain(..).collect();
        while let Ok(v) = self.rx.try_recv() {
            out.push(v);
        }
        out
    }

    pub fn kill(&mut self) {
        self.dead = true;
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
