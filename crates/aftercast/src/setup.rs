//! `aftercast setup`: installs the faster-whisper transcriber and Tesseract OCR (tesserocr
//! wheel plus `eng` data) under `<home>/tools` with uv.
//! uv brings its own Python 3.12 (`tools/python`), so the system Python does not matter.
//! Tool output goes to `<home>/logs/setup.log`.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ac_core::Home;
use ac_core::config::{Config, Transcriber};
use ac_core::exit;
use indicatif::{ProgressBar, ProgressStyle};

use crate::error::{AppError, Result};

pub fn run(home: &Home, config_path: &Path) -> Result<u8> {
    let config = Config::load(config_path)?;
    let package = match config.post.transcriber {
        Transcriber::FasterWhisper => "faster-whisper",
        Transcriber::Whisperx => "whisperx",
        Transcriber::WhisperCpp => {
            println!("transcriber is whisper-cpp: install whisper-cli and a ggml model yourself");
            return Ok(exit::OK);
        }
    };
    fs::create_dir_all(home.logs_dir())?;
    let log = home.logs_dir().join("setup.log");
    fs::write(&log, "")?;
    println!(
        "installing into {} (log: {})",
        home.join("tools").display(),
        log.display()
    );

    step(&log, "uv", Command::new("uv").arg("--version")).map_err(|_| {
        AppError::Usage(
            "uv not found on PATH; install it with `winget install astral-sh.uv` and rerun".into(),
        )
    })?;

    let python = home.whisper_python();
    if !python.exists() {
        step(
            &log,
            "Python 3.12 venv",
            Command::new("uv")
                .args(["venv", "--python", "3.12"])
                .arg(home.join("tools/whisper"))
                .env("UV_PYTHON_INSTALL_DIR", home.join("tools/python")),
        )?;
    }

    let mut install = Command::new("uv");
    install
        .args(["pip", "install", "--upgrade", "--python"])
        .arg(&python)
        .args([package, "tesserocr"])
        // faster-whisper 1.2 passes av.open(metadata_errors=...), which PyAV 16 removed.
        .arg("av<16");
    if !cfg!(target_os = "macos") {
        // cuBLAS 12 and cuDNN 9 for ctranslate2; transcribe.py puts their DLLs on the path.
        install.args(["nvidia-cublas-cu12", "nvidia-cudnn-cu12==9.*"]);
    }
    step(
        &log,
        &format!("{package}, tesserocr, CUDA libraries"),
        &mut install,
    )?;

    let helpers = ac_post::write_helpers(home).map_err(AppError::Usage)?;
    step(
        &log,
        "tesseract language data (eng)",
        Command::new(&python)
            .arg(helpers.join("ocr.py"))
            .args(["--download", "--tessdata"])
            .arg(home.tessdata_dir()),
    )?;
    let model = &config.post.whisper_model;
    step(
        &log,
        &format!("model {model}"),
        Command::new(&python)
            .arg(helpers.join("transcribe.py"))
            .args(["--download", "--model", model, "--models"])
            .arg(home.models_dir()),
    )?;

    let cuda = Command::new(&python)
        .args([
            "-c",
            "import ctranslate2; print(ctranslate2.get_cuda_device_count())",
        ])
        .output()?;
    let devices = String::from_utf8_lossy(&cuda.stdout).trim().to_owned();
    match devices.parse::<u32>() {
        Ok(n) if n > 0 => println!("done: {n} CUDA device(s) visible"),
        _ => println!(
            "done, but no CUDA device is visible: transcription will fail on device cuda (check the NVIDIA driver)"
        ),
    }
    Ok(exit::OK)
}

/// Runs one step behind a spinner, appending its output to `log`.
fn step(log: &Path, label: &str, cmd: &mut Command) -> Result<()> {
    let spinner = ProgressBar::new_spinner()
        .with_style(ProgressStyle::with_template("{spinner} {msg} {elapsed}").expect("template"))
        .with_message(label.to_owned());
    spinner.enable_steady_tick(Duration::from_millis(120));
    let file = fs::OpenOptions::new().append(true).open(log)?;
    let start = Instant::now();
    let status = cmd
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .status();
    spinner.finish_and_clear();
    let secs = start.elapsed().as_secs();
    match status {
        Ok(s) if s.success() => {
            println!("ok    {label} ({secs} s)");
            Ok(())
        }
        Ok(s) => Err(AppError::Usage(format!(
            "{label} failed ({s}); see {}",
            log.display()
        ))),
        Err(e) => Err(AppError::Usage(format!("{label}: {e}"))),
    }
}
