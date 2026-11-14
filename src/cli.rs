//! Command line surface, preserving the verified prototype flags.

use anyhow::{Result, bail};
use clap::Parser;
use std::path::PathBuf;

use crate::run::Options;

#[derive(Debug, Parser)]
#[command(
    name = "sama-display",
    version,
    about = "Read-only CPU/GPU temperature display for the SAMA 480x480 USB cooler screen"
)]
pub struct Args {
    /// Exit after one acknowledged frame.
    #[arg(long, conflicts_with = "frames")]
    pub once: bool,

    /// Exit after N acknowledged frames (three consecutive errors fail).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub frames: Option<u64>,

    /// Minimum frame start-to-start interval in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 3.0, value_parser = parse_positive)]
    pub interval: f64,

    /// Counterclockwise rotation in degrees.
    #[arg(long, value_name = "DEGREES", default_value_t = 90, value_parser = parse_rotation)]
    pub rotation: u16,

    /// State directory for status.json and latest-frame.png.
    #[arg(long, value_name = "PATH")]
    pub state_dir: Option<PathBuf>,
}

impl Args {
    pub fn options(&self) -> Result<Options> {
        let frames = if self.once {
            1
        } else {
            self.frames.unwrap_or(0)
        };
        let state_dir = match &self.state_dir {
            Some(path) => path.clone(),
            None => default_state_dir()?,
        };
        Ok(Options {
            frames,
            interval: self.interval,
            rotation: self.rotation,
            state_dir,
        })
    }
}

fn parse_positive(value: &str) -> std::result::Result<f64, String> {
    let number: f64 = value
        .parse()
        .map_err(|_| format!("invalid number: {value}"))?;
    if !number.is_finite() || number <= 0.0 {
        return Err("must be finite and positive".to_string());
    }
    Ok(number)
}

fn parse_count(value: &str) -> std::result::Result<u64, String> {
    let number: u64 = value
        .parse()
        .map_err(|_| format!("invalid count: {value}"))?;
    if number < 1 {
        return Err("must be at least 1".to_string());
    }
    Ok(number)
}

fn parse_rotation(value: &str) -> std::result::Result<u16, String> {
    let degrees: u16 = value
        .parse()
        .map_err(|_| format!("invalid rotation: {value}"))?;
    if !matches!(degrees, 0 | 90 | 180 | 270) {
        return Err("rotation must be 0, 90, 180 or 270".to_string());
    }
    Ok(degrees)
}

fn default_state_dir_from(
    xdg_state_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    let base = match xdg_state_home {
        Some(path) if !path.as_os_str().is_empty() => path,
        _ => match home {
            Some(path) if !path.as_os_str().is_empty() => path.join(".local/state"),
            _ => bail!("HOME is not set; pass --state-dir or set XDG_STATE_HOME"),
        },
    };
    Ok(base.join("sama-display"))
}

/// `${XDG_STATE_HOME:-$HOME/.local/state}/sama-display`, matching the prototype.
pub fn default_state_dir() -> Result<PathBuf> {
    default_state_dir_from(
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> std::result::Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("sama-display").chain(argv.iter().copied()))
    }

    #[test]
    fn cli_defaults_validation_and_once() {
        let args = parse(&[]).expect("defaults");
        assert_eq!((args.interval, args.rotation, args.frames), (3.0, 90, None));
        assert!(parse(&["--once"]).expect("once").once);
        assert_eq!(parse(&["--frames", "2"]).expect("frames").frames, Some(2));
        for invalid in [
            vec!["--frames", "0"],
            vec!["--interval", "nan"],
            vec!["--interval", "0"],
            vec!["--interval", "-1"],
            vec!["--rotation", "45"],
            vec!["--once", "--frames", "2"],
            vec!["--unknown"],
        ] {
            assert!(parse(&invalid).is_err(), "{invalid:?} should be rejected");
        }
        let options = parse(&["--once", "--state-dir", "/tmp/x"])
            .expect("once options")
            .options()
            .expect("options");
        assert_eq!(options.frames, 1);
        assert_eq!(options.state_dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn state_directory_env_precedence_and_missing_home() {
        assert_eq!(
            default_state_dir_from(
                Some(PathBuf::from("/xdg")),
                Some(PathBuf::from("/home/user"))
            )
            .unwrap(),
            PathBuf::from("/xdg/sama-display")
        );
        assert_eq!(
            default_state_dir_from(None, Some(PathBuf::from("/home/user"))).unwrap(),
            PathBuf::from("/home/user/.local/state/sama-display")
        );
        let error = default_state_dir_from(None, None).unwrap_err();
        assert!(error.to_string().contains("HOME is not set"));
    }
}
