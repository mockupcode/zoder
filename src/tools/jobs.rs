use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::session::Session;

use super::{arg_bool, arg_str, arg_u64};

struct Job {
    command: String,
    buf: Mutex<String>,
    done: AtomicBool,
    ok: AtomicBool,
    cancel: Arc<AtomicBool>,
}

static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn bash(session: &Session, args: &Value, cancel: &AtomicBool) -> (bool, String) {
    let Some(command) = arg_str(args, "command") else {
        return (false, "command is required".into());
    };
    let bg = arg_bool(args, "run_in_background").unwrap_or(false);
    let auto_s = arg_u64(args, "auto_background_after")
        .unwrap_or(60)
        .clamp(1, 600);
    let timeout = Duration::from_millis(
        arg_u64(args, "timeout_ms")
            .unwrap_or(60_000)
            .clamp(1_000, 300_000),
    );
    let id = format!("sh-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let job = Arc::new(Job {
        command: command.to_string(),
        buf: Mutex::new(String::new()),
        done: AtomicBool::new(false),
        ok: AtomicBool::new(false),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    jobs().lock().unwrap().insert(id.clone(), job.clone());
    let cwd = session.cwd.clone();
    let cmd = command.to_string();
    let job_run = job.clone();
    tokio::spawn(async move {
        run_shell(cwd, cmd, job_run, timeout).await;
    });
    if bg {
        tokio::time::sleep(Duration::from_millis(400)).await;
        if job.done.load(Ordering::Relaxed) {
            let out = job.buf.lock().unwrap().clone();
            jobs().lock().unwrap().remove(&id);
            return (job.ok.load(Ordering::Relaxed), out);
        }
        return (
            true,
            format!("Background shell started with ID: {id}\n\nUse job_output to view output or job_kill to terminate."),
        );
    }
    let wait = Duration::from_secs(auto_s);
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            job.cancel.store(true, Ordering::Relaxed);
            return (false, "cancelled".into());
        }
        if job.done.load(Ordering::Relaxed) {
            let out = job.buf.lock().unwrap().clone();
            jobs().lock().unwrap().remove(&id);
            return (job.ok.load(Ordering::Relaxed), out);
        }
        if start.elapsed() >= wait {
            return (
                true,
                format!("Command is taking longer than expected and has been moved to background.\n\nBackground shell ID: {id}\n\nUse job_output to view output or job_kill to terminate."),
            );
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

pub async fn job_output(args: &Value, cancel: &AtomicBool) -> (bool, String) {
    let Some(id) = arg_str(args, "shell_id").or_else(|| arg_str(args, "id")) else {
        return (false, "missing shell_id".into());
    };
    let wait = arg_bool(args, "wait").unwrap_or(false);
    let job = {
        let g = jobs().lock().unwrap();
        g.get(id).cloned()
    };
    let Some(job) = job else {
        return (false, format!("background shell not found: {id}"));
    };
    if wait {
        while !job.done.load(Ordering::Relaxed) {
            if cancel.load(Ordering::Relaxed) {
                return (false, "cancelled".into());
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }
    let out = job.buf.lock().unwrap().clone();
    let status = if job.done.load(Ordering::Relaxed) {
        "completed"
    } else {
        "running"
    };
    let body = if out.trim().is_empty() {
        "no output".into()
    } else {
        out
    };
    (
        true,
        format!("Status: {status}\nCommand: {}\n\n{body}", job.command),
    )
}

pub fn job_kill(args: &Value) -> (bool, String) {
    let Some(id) = arg_str(args, "shell_id").or_else(|| arg_str(args, "id")) else {
        return (false, "missing shell_id".into());
    };
    let job = {
        let mut g = jobs().lock().unwrap();
        g.remove(id)
    };
    let Some(job) = job else {
        return (false, format!("background shell not found: {id}"));
    };
    job.cancel.store(true, Ordering::Relaxed);
    (true, format!("terminated {id}"))
}

async fn run_shell(cwd: std::path::PathBuf, command: String, job: Arc<Job>, timeout: Duration) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            *job.buf.lock().unwrap() = e.to_string();
            job.done.store(true, Ordering::Relaxed);
            return;
        }
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let read_out = async {
        let mut b = Vec::new();
        if let Some(ref mut r) = stdout {
            let _ = r.read_to_end(&mut b).await;
        }
        b
    };
    let read_err = async {
        let mut b = Vec::new();
        if let Some(ref mut r) = stderr {
            let _ = r.read_to_end(&mut b).await;
        }
        b
    };
    let start = Instant::now();
    let (status, out, err) = tokio::select! {
        status = child.wait() => {
            let (out, err) = tokio::join!(read_out, read_err);
            (status, out, err)
        }
        _ = wait_flag(&job.cancel) => {
            kill_child(&mut child);
            let _ = child.wait().await;
            job.done.store(true, Ordering::Relaxed);
            *job.buf.lock().unwrap() = "cancelled".into();
            return;
        }
        _ = tokio::time::sleep(timeout) => {
            kill_child(&mut child);
            let _ = child.wait().await;
            job.done.store(true, Ordering::Relaxed);
            *job.buf.lock().unwrap() = format!("timed out after {timeout:?}");
            return;
        }
    };
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err));
    }
    if text.len() > 30_000 {
        text.truncate(15_000);
        text.push_str("\n\n... truncated ...\n\n");
    }
    let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
    let ms = start.elapsed().as_millis();
    let body = if text.trim().is_empty() {
        format!("exit {code} ({ms}ms)")
    } else {
        format!("{text}\nexit {code} ({ms}ms)")
    };
    *job.buf.lock().unwrap() = body;
    job.ok.store(code == 0, Ordering::Relaxed);
    job.done.store(true, Ordering::Relaxed);
}

async fn wait_flag(flag: &AtomicBool) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

fn kill_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.start_kill();
}
