//! Reconnect-aware display loop and acknowledged-frame accounting.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use image::RgbaImage;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clock::{Clock, Stop};
use crate::encode;
use crate::state::{Publisher, Status};
use crate::transport::Screen;

/// Finite runs fail after this many consecutive failed cycles.
pub const FINITE_FAILURE_LIMIT: u32 = 3;
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Options {
    pub frames: u64,
    pub interval: f64,
    pub rotation: u16,
    pub state_dir: PathBuf,
}

impl Options {
    fn interval_duration(&self) -> Duration {
        Duration::from_secs_f64(self.interval)
    }
}

/// Read-only CPU/GPU temperature source.
pub trait TemperatureSource {
    fn read(&self) -> (Option<f64>, Option<f64>);
}

/// Renders one upright frame; rotation is applied inside the implementation.
pub trait Renderer {
    fn render(
        &self,
        cpu: Option<f64>,
        gpu: Option<f64>,
        clock: &str,
        rotation: u16,
    ) -> Result<RgbaImage>;
}

/// Opens display connections and attempts the best-effort CDC bridge wake.
pub trait Link {
    fn wake(&self) -> Result<()>;
    fn open(&self) -> Result<Box<dyn Screen>>;
}

pub struct Deps<'a> {
    pub link: &'a dyn Link,
    pub temperatures: &'a dyn TemperatureSource,
    pub renderer: &'a dyn Renderer,
    pub publisher: &'a dyn Publisher,
    pub clock: &'a dyn Clock,
    pub stop: &'a dyn Stop,
}

struct Connection {
    screen: Box<dyn Screen>,
    identity: String,
}

enum Cycle {
    Frame { started: Duration },
    Cancelled,
}

fn utc_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn elapsed(from: Duration, to: Duration) -> f64 {
    round6(to.as_secs_f64() - from.as_secs_f64())
}

/// Continues the persisted acknowledged-frame sequence when a valid status
/// file exists; anything unreadable restarts at zero.
pub fn load_sequence(state_dir: &Path) -> u64 {
    let Ok(text) = fs::read_to_string(state_dir.join("status.json")) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    match value.get("sequence") {
        Some(serde_json::Value::Number(number)) => number
            .as_u64()
            .or_else(|| {
                number
                    .as_i64()
                    .filter(|value| *value > 0)
                    .map(|value| value as u64)
            })
            .unwrap_or(0),
        _ => 0,
    }
}

fn perform_cycle(
    options: &Options,
    deps: &Deps,
    connection: &mut Option<Connection>,
    sequence: &mut u64,
) -> Result<Cycle> {
    if connection.is_none() {
        // DTR is only applied to the exact known sleeping identity.
        if let Err(error) = deps.link.wake() {
            log::warn!("Bridge wake unavailable: {error:#}");
        }
        if deps.stop.is_set() {
            return Ok(Cycle::Cancelled);
        }
        let mut session = deps.link.open().context("open display")?;
        let identity = match session.handshake() {
            Ok(identity) => identity,
            Err(error) => {
                session.close();
                return Err(error).context("handshake");
            }
        };
        log::info!("Connected {identity}");
        *connection = Some(Connection {
            screen: session,
            identity,
        });
    }
    let started = deps.clock.now();
    let started_at = utc_now();
    let (cpu, gpu) = deps.temperatures.read();
    let sampled_at = utc_now();
    if cpu.is_none() || gpu.is_none() {
        log::warn!("Sensor unavailable: CPU={cpu:?} GPU={gpu:?}");
    }
    let clock_text = chrono::Local::now().format("%H:%M:%S").to_string();
    let image = deps
        .renderer
        .render(cpu, gpu, &clock_text, options.rotation)?;
    let sending = deps.clock.now();
    let bgra = encode::bgra_from_rgba(&image);
    let active = connection.as_mut().expect("connection established");
    let ack = active.screen.frame(&bgra)?;
    let acknowledged = deps.clock.now();
    *sequence += 1;
    let record = Status {
        sequence: *sequence,
        started_at,
        sampled_at,
        acknowledged_at: utc_now(),
        ack_latency_seconds: elapsed(sending, acknowledged),
        frame_latency_seconds: elapsed(started, acknowledged),
        cpu_celsius: cpu,
        gpu_celsius: gpu,
        rotation: options.rotation,
        display_id: active.identity.clone(),
        ack,
        image: "latest-frame.png".to_string(),
    };
    deps.publisher
        .publish(&options.state_dir, &image, &record)
        .context("publish acknowledged frame")?;
    log::info!(
        "ACK frame={} CPU={:?} GPU={:?} latency={:.3}s",
        record.sequence,
        cpu,
        gpu,
        record.ack_latency_seconds
    );
    Ok(Cycle::Frame { started })
}

/// Runs until stopped or until N acknowledged frames; reconnects before retry.
pub fn run(options: &Options, deps: &Deps) -> Result<u64> {
    fs::create_dir_all(&options.state_dir)
        .with_context(|| format!("create {}", options.state_dir.display()))?;
    let mut sequence = load_sequence(&options.state_dir);
    let mut completed = 0u64;
    let mut failures = 0u32;
    let mut connection: Option<Connection> = None;
    let interval = options.interval_duration();
    while !deps.stop.is_set() && (options.frames == 0 || completed < options.frames) {
        match perform_cycle(options, deps, &mut connection, &mut sequence) {
            Ok(Cycle::Frame { started }) => {
                completed += 1;
                failures = 0;
                if options.frames > 0 && completed >= options.frames {
                    break;
                }
                let target = started + interval;
                let now = deps.clock.now();
                deps.stop.wait(target.saturating_sub(now));
            }
            Ok(Cycle::Cancelled) => break,
            Err(error) => {
                if let Some(mut active) = connection.take() {
                    active.screen.close();
                }
                if deps.stop.is_set() {
                    log::debug!("Stopping after cancellation: {error:#}");
                    break;
                }
                log::error!("Display cycle failed; releasing before reconnect: {error:#}");
                failures += 1;
                if options.frames > 0 && failures >= FINITE_FAILURE_LIMIT {
                    return Err(error);
                }
                let backoff = Duration::from_secs(1u64 << (failures - 1).min(5)).min(MAX_BACKOFF);
                deps.stop.wait(backoff);
            }
        }
    }
    if let Some(mut active) = connection.take() {
        active.screen.close();
    }
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Publisher;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeClock(Arc<AtomicU64>);

    impl FakeClock {
        fn new() -> Self {
            Self(Arc::new(AtomicU64::new(0)))
        }
        fn advance(&self, duration: Duration) {
            self.0
                .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    struct FakeStop {
        flag: Arc<AtomicBool>,
        clock: FakeClock,
    }

    impl FakeStop {
        fn new(clock: FakeClock) -> Self {
            Self {
                flag: Arc::new(AtomicBool::new(false)),
                clock,
            }
        }
    }

    impl Stop for FakeStop {
        fn is_set(&self) -> bool {
            self.flag.load(Ordering::SeqCst)
        }

        fn wait(&self, timeout: Duration) {
            if !self.is_set() {
                self.clock.advance(timeout);
            }
        }
    }

    #[derive(Default)]
    struct World {
        frame_starts: Mutex<Vec<Duration>>,
        closed: Mutex<Vec<u64>>,
        devices: AtomicU64,
        frame_calls: AtomicU64,
        handshakes: AtomicU64,
        fail_frames: AtomicU64,
        cancel_handshake: AtomicBool,
        stop: Arc<AtomicBool>,
    }

    impl World {
        fn new(stop: Arc<AtomicBool>) -> Self {
            Self {
                stop,
                ..Self::default()
            }
        }
    }

    struct TestScreen {
        world: Arc<World>,
        id: u64,
        clock: FakeClock,
    }

    impl Screen for TestScreen {
        fn handshake(&mut self) -> Result<String> {
            self.world.handshakes.fetch_add(1, Ordering::SeqCst);
            if self.world.cancel_handshake.load(Ordering::SeqCst) {
                self.world.stop.store(true, Ordering::SeqCst);
                return Err(anyhow::anyhow!("SIGTERM"));
            }
            Ok("chs_5inch.dev1_rom1.89".to_string())
        }

        fn frame(&mut self, _bgra: &[u8]) -> Result<String> {
            self.world
                .frame_starts
                .lock()
                .expect("starts")
                .push(self.clock.now());
            let call = self.world.frame_calls.fetch_add(1, Ordering::SeqCst);
            self.clock.advance(Duration::from_millis(500));
            if call < self.world.fail_frames.load(Ordering::SeqCst) {
                anyhow::bail!("lost ack");
            }
            Ok("full_png_sucess".to_string())
        }

        fn close(&mut self) {
            self.world.closed.lock().expect("closed").push(self.id);
        }
    }

    struct TestBedrock {
        world: Arc<World>,
        clock: FakeClock,
    }

    impl Link for TestBedrock {
        fn wake(&self) -> Result<()> {
            Ok(())
        }

        fn open(&self) -> Result<Box<dyn Screen>> {
            let id = self.world.devices.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TestScreen {
                world: self.world.clone(),
                id,
                clock: self.clock.clone(),
            }))
        }
    }

    struct MockRenderer;

    impl Renderer for MockRenderer {
        fn render(
            &self,
            _cpu: Option<f64>,
            _gpu: Option<f64>,
            _clock: &str,
            _rotation: u16,
        ) -> Result<RgbaImage> {
            Ok(RgbaImage::new(2, 2))
        }
    }

    struct MockTemps(Option<f64>, Option<f64>);

    impl TemperatureSource for MockTemps {
        fn read(&self) -> (Option<f64>, Option<f64>) {
            (self.0, self.1)
        }
    }

    struct Recorder {
        statuses: Arc<Mutex<Vec<Status>>>,
        fail: bool,
    }

    impl Publisher for Recorder {
        fn publish(&self, _dir: &Path, _image: &RgbaImage, status: &Status) -> Result<()> {
            if self.fail {
                anyhow::bail!("disk full");
            }
            self.statuses.lock().expect("statuses").push(status.clone());
            Ok(())
        }
    }

    fn make_options(dir: &Path, frames: u64) -> Options {
        Options {
            frames,
            interval: 3.0,
            rotation: 90,
            state_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn reconnect_cadence_ack_only_state_and_release() {
        let clock = FakeClock::new();
        let stop = FakeStop::new(clock.clone());
        let world = Arc::new(World::new(stop.flag.clone()));
        world.fail_frames.store(1, Ordering::SeqCst);
        let link = TestBedrock {
            world: world.clone(),
            clock: clock.clone(),
        };
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let publisher = Recorder {
            statuses: statuses.clone(),
            fail: false,
        };
        let temps = MockTemps(Some(55.0), Some(49.0));
        let deps = Deps {
            link: &link,
            temperatures: &temps,
            renderer: &MockRenderer,
            publisher: &publisher,
            clock: &clock,
            stop: &stop,
        };
        let directory = tempfile::tempdir().expect("tempdir");
        let options = make_options(directory.path(), 2);
        assert_eq!(run(&options, &deps).expect("run"), 2);

        let statuses = statuses.lock().expect("statuses");
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].sequence, 1);
        assert_eq!(statuses[1].sequence, 2);
        assert_eq!(statuses[0].rotation, 90);
        assert_eq!(statuses[0].cpu_celsius, Some(55.0));
        assert_eq!(statuses[0].gpu_celsius, Some(49.0));
        assert_eq!(statuses[0].ack_latency_seconds, 0.5);
        assert_eq!(statuses[0].ack, "full_png_sucess");
        drop(statuses);

        let starts = world.frame_starts.lock().expect("starts").clone();
        assert_eq!(starts.len(), 3);
        assert_eq!(starts[2] - starts[1], Duration::from_secs(3));
        assert_eq!(world.devices.load(Ordering::SeqCst), 2);
        let closed = world.closed.lock().expect("closed").clone();
        assert_eq!(closed.len(), 2, "both connections released");
        assert_eq!(
            fs::read_dir(directory.path())
                .unwrap()
                .filter(|entry| entry
                    .as_ref()
                    .map(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp"))
                    .unwrap_or(false))
                .count(),
            0
        );

        // Unavailable sensors publish nulls and continue the sequence.
        let missing = MockTemps(None, None);
        let deps = Deps {
            temperatures: &missing,
            ..deps
        };
        fs::write(directory.path().join("status.json"), r#"{"sequence": 2}"#).unwrap();
        let options = make_options(directory.path(), 1);
        run(&options, &deps).expect("second run");
        let statuses = publisher.statuses.lock().expect("statuses");
        assert_eq!(statuses.last().expect("status").sequence, 3);
        assert_eq!(statuses.last().expect("status").cpu_celsius, None);
    }

    #[test]
    fn failed_publication_is_not_a_successful_finite_run() {
        let clock = FakeClock::new();
        let stop = FakeStop::new(clock.clone());
        let world = Arc::new(World::new(stop.flag.clone()));
        let link = TestBedrock {
            world: world.clone(),
            clock: clock.clone(),
        };
        let publisher = Recorder {
            statuses: Arc::default(),
            fail: true,
        };
        let temps = MockTemps(Some(55.0), Some(49.0));
        let deps = Deps {
            link: &link,
            temperatures: &temps,
            renderer: &MockRenderer,
            publisher: &publisher,
            clock: &clock,
            stop: &stop,
        };
        let directory = tempfile::tempdir().expect("tempdir");
        let error =
            run(&make_options(directory.path(), 1), &deps).expect_err("disk failure must fail");
        assert!(format!("{error:#}").contains("disk full"));
        assert_eq!(world.devices.load(Ordering::SeqCst), 3);
        assert_eq!(world.closed.lock().expect("closed").len(), 3);
        assert!(publisher.statuses.lock().expect("statuses").is_empty());
    }

    #[test]
    fn cancellation_releases_a_partially_initialized_session() {
        let clock = FakeClock::new();
        let stop = FakeStop::new(clock.clone());
        let world = Arc::new(World::new(stop.flag.clone()));
        world.cancel_handshake.store(true, Ordering::SeqCst);
        let link = TestBedrock {
            world: world.clone(),
            clock: clock.clone(),
        };
        let publisher = Recorder {
            statuses: Arc::default(),
            fail: false,
        };
        let temps = MockTemps(Some(55.0), Some(49.0));
        let deps = Deps {
            link: &link,
            temperatures: &temps,
            renderer: &MockRenderer,
            publisher: &publisher,
            clock: &clock,
            stop: &stop,
        };
        let directory = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            run(&make_options(directory.path(), 0), &deps).expect("run"),
            0
        );
        assert!(!directory.path().join("status.json").exists());
        assert_eq!(world.closed.lock().expect("closed").len(), 1);
    }

    #[test]
    fn state_directory_is_created_and_sequence_continues() {
        let clock = FakeClock::new();
        let stop = FakeStop::new(clock.clone());
        let world = Arc::new(World::new(stop.flag.clone()));
        let link = TestBedrock {
            world: world.clone(),
            clock: clock.clone(),
        };
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let publisher = Recorder {
            statuses: statuses.clone(),
            fail: false,
        };
        let temps = MockTemps(Some(42.0), Some(43.0));
        let deps = Deps {
            link: &link,
            temperatures: &temps,
            renderer: &MockRenderer,
            publisher: &publisher,
            clock: &clock,
            stop: &stop,
        };
        let directory = tempfile::tempdir().expect("tempdir");
        let state_dir = directory.path().join("nested").join("state");
        // A missing state directory is created on demand.
        run(&make_options(&state_dir, 1), &deps).expect("create state directory");
        assert!(state_dir.is_dir());

        // A valid previous status continues the persisted sequence.
        fs::write(
            state_dir.join("status.json"),
            r#"{"sequence": 41, "cpu_celsius": 1}"#,
        )
        .unwrap();
        run(&make_options(&state_dir, 1), &deps).expect("run");
        assert_eq!(
            statuses.lock().expect("statuses")[1].sequence,
            42,
            "sequence continues from a valid previous status"
        );
    }
}
