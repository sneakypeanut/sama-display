//! Read-only Intel package and NVIDIA GPU temperature sources.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const NVIDIA_ARGS: [&str; 2] = [
    "--query-gpu=temperature.gpu",
    "--format=csv,noheader,nounits",
];
pub const NVIDIA_BUDGET: Duration = Duration::from_secs(2);

/// Command invocation seam so sensor tests never spawn real tooling unless
/// they explicitly test the bounded runner.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<String>;
}

/// Runs a child with a hard deadline and no inherited stderr.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<String> {
        run_bounded(program, args, timeout)
    }
}

pub fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    use std::io::Read;
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {program}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut output = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                stdout.read_to_string(&mut output)?;
            }
            if !status.success() {
                bail!("{program} exited with {status}");
            }
            return Ok(output);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} exceeded {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn package_temperatures(hwmon: &Path) -> Vec<f64> {
    let mut packages = Vec::new();
    let entries = match fs::read_dir(hwmon) {
        Ok(entries) => entries,
        Err(_) => return packages,
    };
    for entry in entries.flatten() {
        let directory = entry.path();
        let is_coretemp = fs::read_to_string(directory.join("name"))
            .map(|name| name.trim() == "coretemp")
            .unwrap_or(false);
        if !is_coretemp {
            continue;
        }
        let labels = match fs::read_dir(&directory) {
            Ok(labels) => labels,
            Err(_) => continue,
        };
        for label in labels.flatten() {
            let path = label.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if !name.ends_with("_label") {
                continue;
            }
            let label_text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => continue,
            };
            if !label_text.trim().starts_with("Package id ") {
                continue;
            }
            let input = path.with_file_name(name.replace("_label", "_input"));
            let value = fs::read_to_string(input)
                .ok()
                .and_then(|text| text.trim().parse::<f64>().ok())
                .map(|millidegrees| millidegrees / 1000.0);
            if let Some(value) = value
                && (-40.0..=150.0).contains(&value)
            {
                packages.push(value);
            }
        }
    }
    packages
}

/// Reads the hottest coretemp package and the first NVIDIA GPU.
/// Missing values are `None`, never fabricated.
pub fn temperatures(hwmon: &Path, runner: &dyn CommandRunner) -> (Option<f64>, Option<f64>) {
    let packages = package_temperatures(hwmon);
    let cpu = packages.iter().copied().reduce(f64::max);
    let gpu = runner
        .run("nvidia-smi", &NVIDIA_ARGS, NVIDIA_BUDGET)
        .ok()
        .and_then(|output| output.lines().next().map(str::trim).map(str::to_string))
        .and_then(|line| line.parse::<f64>().ok())
        .filter(|value| value.is_finite() && (-40.0..=150.0).contains(value));
    (cpu, gpu)
}

pub struct SystemSensors {
    hwmon: PathBuf,
    runner: Box<dyn CommandRunner>,
}

impl SystemSensors {
    pub fn new(hwmon: impl Into<PathBuf>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            hwmon: hwmon.into(),
            runner,
        }
    }

    pub fn system() -> Self {
        Self::new("/sys/class/hwmon", Box::new(SystemRunner))
    }
}

impl crate::run::TemperatureSource for SystemSensors {
    fn read(&self) -> (Option<f64>, Option<f64>) {
        temperatures(&self.hwmon, self.runner.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Recording {
        calls: RefCell<Vec<(String, Vec<String>, Duration)>>,
        output: std::result::Result<String, String>,
    }

    impl CommandRunner for Recording {
        fn run(&self, program: &str, args: &[&str], timeout: Duration) -> Result<String> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|arg| arg.to_string()).collect(),
                timeout,
            ));
            match &self.output {
                Ok(output) => Ok(output.clone()),
                Err(error) => Err(anyhow::anyhow!(error.clone())),
            }
        }
    }

    #[test]
    fn read_only_coretemp_packages_and_nvidia_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let hw = root.path().join("hwmon4");
        fs::create_dir_all(&hw).unwrap();
        fs::write(hw.join("name"), "coretemp\n").unwrap();
        for (index, label, value) in [
            (1, "Package id 0", 55_000),
            (2, "Core 0", 99_000),
            (3, "Package id 1", 60_500),
        ] {
            fs::write(hw.join(format!("temp{index}_label")), label).unwrap();
            fs::write(hw.join(format!("temp{index}_input")), value.to_string()).unwrap();
        }
        let runner = Recording {
            calls: RefCell::new(Vec::new()),
            output: Ok("49\n50\n".to_string()),
        };
        assert_eq!(temperatures(root.path(), &runner), (Some(60.5), Some(49.0)));
        let calls = runner.calls.borrow();
        assert_eq!(calls[0].0, "nvidia-smi");
        assert_eq!(
            calls[0].1,
            vec![
                "--query-gpu=temperature.gpu",
                "--format=csv,noheader,nounits"
            ]
        );
        assert!(calls[0].2 <= Duration::from_secs(2));

        let missing = Recording {
            calls: RefCell::new(Vec::new()),
            output: Err("timed out".to_string()),
        };
        assert_eq!(temperatures(root.path(), &missing), (Some(60.5), None));
        fs::write(hw.join("temp1_input"), "bad").unwrap();
        assert_eq!(temperatures(root.path(), &missing), (Some(60.5), None));
    }

    #[test]
    fn bounded_runner_kills_overrunning_children() {
        let started = Instant::now();
        let error =
            run_bounded("sh", &["-c", "sleep 5"], Duration::from_millis(300)).expect_err("timeout");
        assert!(error.to_string().contains("exceeded"));
        assert!(started.elapsed() < Duration::from_secs(2));

        assert_eq!(
            run_bounded("sh", &["-c", "echo 49"], Duration::from_secs(1)).expect("short command"),
            "49\n"
        );
    }
}
