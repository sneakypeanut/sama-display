//! Serial-scoped libusb transport with bounded writes and RAII release.

use anyhow::{Context as _, Result, anyhow, bail};
use rusb::{Context, DeviceHandle, UsbContext};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock::{Cancellation, Clock};
use crate::run::Link;
use crate::transport::{DisplaySession, Screen, Transport};

pub const VENDOR_ID: u16 = 0x1d6b;
pub const PRODUCT_ID: u16 = 0x0121;
pub const SERIAL: &str = "20080411";

const INTERFACE: u8 = 1;
const ENDPOINT_OUT: u8 = 1;
const ENDPOINT_IN: u8 = 0x81;
const CHUNK: usize = 16_000;
const CHUNK_TIMEOUT: Duration = Duration::from_millis(500);
const WRITE_BUDGET: Duration = Duration::from_secs(20);
const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);

/// PyUSB lifetime owner equivalent: finite I/O and cancellation between writes.
pub struct UsbTransport {
    handle: DeviceHandle<Context>,
    detached: Vec<u8>,
    claimed: bool,
    closed: bool,
    cancel: Arc<dyn Cancellation>,
}

impl UsbTransport {
    /// Discovers the display by VID/PID and serial, detaches kernel drivers and
    /// claims interface 1. Cleanup runs even when a later init step fails.
    pub fn open(cancel: Arc<dyn Cancellation>) -> Result<Self> {
        let context = Context::new().context("create libusb context")?;
        let devices = context.devices().context("list USB devices")?;
        let mut selected = None;
        for device in devices.iter() {
            let descriptor = match device.device_descriptor() {
                Ok(descriptor) => descriptor,
                Err(_) => continue,
            };
            if descriptor.vendor_id() != VENDOR_ID || descriptor.product_id() != PRODUCT_ID {
                continue;
            }
            let handle = match device.open() {
                Ok(handle) => handle,
                Err(_) => continue,
            };
            match handle.read_serial_number_string_ascii(&descriptor) {
                Ok(serial) if serial == SERIAL => {
                    selected = Some(handle);
                    break;
                }
                _ => continue,
            }
        }
        let handle = selected.ok_or_else(|| {
            anyhow!("SAMA display {VENDOR_ID:04x}:{PRODUCT_ID:04x} serial {SERIAL} not found")
        })?;
        let mut transport = Self {
            handle,
            detached: Vec::new(),
            claimed: false,
            closed: false,
            cancel,
        };
        let initialized = (|| -> Result<()> {
            for interface in [0u8, INTERFACE] {
                if transport.handle.kernel_driver_active(interface)? {
                    transport.handle.detach_kernel_driver(interface)?;
                    transport.detached.push(interface);
                }
            }
            transport.handle.claim_interface(INTERFACE)?;
            transport.claimed = true;
            transport
                .handle
                .write_control(0x21, 0x22, 3, 0, &[], CONTROL_TIMEOUT)?;
            Ok(())
        })();
        if let Err(error) = initialized {
            transport.close();
            return Err(error);
        }
        Ok(transport)
    }

    fn cleanup(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if self.claimed {
            if let Err(error) = self.handle.release_interface(INTERFACE) {
                log::warn!("USB release: {error}");
            }
            self.claimed = false;
        }
        for interface in self.detached.drain(..).rev() {
            if let Err(error) = self.handle.attach_kernel_driver(interface) {
                log::warn!("USB reattach interface {interface}: {error}");
            }
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Writes the frame body in 16000-byte bulk chunks and rejects short writes.
pub(crate) fn write_chunks<F>(data: &[u8], mut write: F) -> Result<()>
where
    F: FnMut(&[u8]) -> Result<usize>,
{
    for chunk in data.chunks(CHUNK) {
        if write(chunk)? != chunk.len() {
            bail!("Short USB write");
        }
    }
    Ok(())
}

impl Transport for UsbTransport {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        let Self { handle, cancel, .. } = self;
        let deadline = Instant::now() + WRITE_BUDGET;
        write_chunks(data, |chunk| {
            if cancel.cancelled() {
                bail!("Stopping");
            }
            if Instant::now() >= deadline {
                bail!("USB frame transfer deadline exceeded");
            }
            Ok(handle.write_bulk(ENDPOINT_OUT, chunk, CHUNK_TIMEOUT)?)
        })
    }

    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut buffer = [0u8; 1024];
        let timeout = timeout.max(Duration::from_millis(1));
        match self.handle.read_bulk(ENDPOINT_IN, &mut buffer, timeout) {
            Ok(count) => Ok(buffer[..count].to_vec()),
            Err(rusb::Error::Timeout) => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn close(&mut self) {
        self.cleanup();
    }
}

/// Production link: wake the sleeping CDC bridge, then open a display session.
pub struct UsbLink {
    cancel: Arc<dyn Cancellation>,
    clock: Arc<dyn Clock>,
}

impl UsbLink {
    pub fn new(cancel: Arc<dyn Cancellation>, clock: Arc<dyn Clock>) -> Self {
        Self { cancel, clock }
    }
}

impl Link for UsbLink {
    fn wake(&self) -> Result<()> {
        let woke = crate::wake::wake_bridge(
            std::path::Path::new("/sys/class/tty"),
            std::path::Path::new("/dev"),
            &crate::wake::RealTtyOps,
            &|duration| std::thread::sleep(duration),
        )?;
        if !woke {
            log::debug!("No sleeping SAMA CDC bridge found");
        }
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn Screen>> {
        let transport = UsbTransport::open(self.cancel.clone())?;
        Ok(Box::new(DisplaySession::new(
            Box::new(transport),
            self.clock.clone(),
            self.cancel.clone(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_16000_bytes_and_short_writes_fail() {
        let data = vec![7u8; 32_001];
        let mut sizes = Vec::new();
        write_chunks(&data, |chunk| {
            sizes.push(chunk.len());
            Ok(chunk.len())
        })
        .expect("chunked write");
        assert_eq!(sizes, vec![16_000, 16_000, 1]);

        let error = write_chunks(&data, |_chunk| Ok(0)).unwrap_err();
        assert!(error.to_string().contains("Short USB write"));
    }
}
