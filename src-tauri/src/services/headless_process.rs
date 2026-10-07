use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::async_runtime::{channel, Receiver, Sender};
use tauri_plugin_shell::process::{CommandEvent, TerminatedPayload};

pub struct NativeChild(Arc<Mutex<Child>>);
impl NativeChild {
    pub fn kill(self) -> Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "Child lock failed")?
            .kill()
            .map_err(|e| e.to_string())
    }
}

pub fn sidecar_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("IMMICH_SHUTTLE_SIDECAR") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) {
        "immich-go.exe"
    } else {
        "immich-go"
    };
    let path = executable
        .parent()
        .ok_or("Executable has no directory")?
        .join(name);
    if path.is_file() {
        return Ok(path);
    }
    Err("Bundled immich-go was not found next to the executable. Set IMMICH_SHUTTLE_SIDECAR to its absolute path for a development run.".into())
}

fn drain(mut input: impl Read + Send + 'static, tx: Sender<CommandEvent>, stderr: bool) {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        let mut line = Vec::with_capacity(512);
        while let Ok(n) = input.read(&mut chunk) {
            if n == 0 {
                break;
            }
            for byte in &chunk[..n] {
                if *byte == b'\n' || *byte == b'\r' {
                    if stderr
                        && !line.is_empty()
                        && tx
                            .blocking_send(CommandEvent::Stderr(std::mem::take(&mut line)))
                            .is_err()
                    {
                        return;
                    }
                    line.clear();
                } else if stderr && line.len() < 4096 {
                    line.push(*byte);
                }
            }
        }
        if stderr && !line.is_empty() {
            let _ = tx.blocking_send(CommandEvent::Stderr(line));
        }
    });
}

pub fn spawn(args: Vec<String>) -> Result<(Receiver<CommandEvent>, NativeChild), String> {
    let mut command = Command::new(sidecar_path()?);
    command
        .args(args)
        .env("GODEBUG", "netdns=cgo")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start immich-go: {e}"))?;
    let (tx, rx) = channel(64);
    drain(
        child.stdout.take().ok_or("Missing stdout")?,
        tx.clone(),
        false,
    );
    drain(
        child.stderr.take().ok_or("Missing stderr")?,
        tx.clone(),
        true,
    );
    let child = Arc::new(Mutex::new(child));
    let waiter = child.clone();
    std::thread::spawn(move || loop {
        let result = waiter.lock().expect("child lock").try_wait();
        match result {
            Ok(Some(status)) => {
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal()
                };
                #[cfg(not(unix))]
                let signal = None;
                let _ = tx.blocking_send(CommandEvent::Terminated(TerminatedPayload {
                    code: status.code(),
                    signal,
                }));
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = tx.blocking_send(CommandEvent::Error(error.to_string()));
                break;
            }
        }
    });
    Ok((rx, NativeChild(child)))
}
