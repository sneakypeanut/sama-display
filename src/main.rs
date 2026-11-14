//! Process entry point: argument parsing, single-writer lock, signals and run.

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;

use sama_display::cli::Args;
use sama_display::clock::{Cancellation, Clock, MonoClock, SignalStop, Stop};
use sama_display::render::SystemRenderer;
use sama_display::run::{self, Deps};
use sama_display::sensors::SystemSensors;
use sama_display::state::FilePublisher;
use sama_display::usb::UsbLink;

struct StderrLogger;

static LOGGER: StderrLogger = StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            eprintln!("{} {} {}", timestamp, record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

fn init_logger() {
    let _ = log::set_logger(&LOGGER);
    let level = match std::env::var("RUST_LOG").ok().as_deref() {
        Some("error") => log::LevelFilter::Error,
        Some("warn") => log::LevelFilter::Warn,
        Some("debug") => log::LevelFilter::Debug,
        Some("trace") => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    log::set_max_level(level);
}

/// Advisory lock so only one service or manual run owns a state directory.
fn acquire_lock(state_dir: &Path) -> Result<File> {
    let path = state_dir.join("display.lock");
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    // SAFETY: flock only reads the file descriptor; the File stays alive until
    // the end of try_main, which keeps the lock held.
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("Another display process holds {}", path.display());
        }
        return Err(error).with_context(|| format!("lock {}", path.display()));
    }
    Ok(lock)
}

fn try_main(args: Args) -> Result<()> {
    let options = args.options()?;
    fs::create_dir_all(&options.state_dir)
        .with_context(|| format!("create {}", options.state_dir.display()))?;
    let lock = acquire_lock(&options.state_dir)?;
    SignalStop::install()?;
    let clock: Arc<dyn Clock> = Arc::new(MonoClock::new());
    let stop = Arc::new(SignalStop);
    let cancel: Arc<dyn Cancellation> = stop.clone();
    let renderer = SystemRenderer::load()?;
    let sensors = SystemSensors::system();
    let publisher = FilePublisher;
    let link = UsbLink::new(cancel, clock.clone());
    let deps = Deps {
        link: &link,
        temperatures: &sensors,
        renderer: &renderer,
        publisher: &publisher,
        clock: clock.as_ref(),
        stop: stop.as_ref() as &dyn Stop,
    };
    let frames = run::run(&options, &deps)?;
    log::info!(
        "Display released after {frames} acknowledged frame(s); no fan or pump controls changed"
    );
    drop(lock);
    Ok(())
}

fn run_main() -> i32 {
    init_logger();
    let args = Args::parse();
    match try_main(args) {
        Ok(()) => 0,
        Err(error) => {
            log::error!("Display stopped with error: {error:#}");
            1
        }
    }
}

fn main() {
    std::process::exit(run_main());
}
