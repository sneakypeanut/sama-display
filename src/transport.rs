//! Display protocol: bounded reply polling, handshake and single in-flight frame.

use anyhow::{Context, Result, bail};
use std::sync::Arc;

use crate::clock::{Cancellation, Clock};
use crate::encode::{self, BLOCK};

/// Accepted frame acknowledgements; generic `success` is deliberately absent.
pub const FRAME_ACKS: [&str; 2] = ["full_png_sucess", "full_png_success"];
pub const IDENTITY: &str = "chs_5inch.dev1_rom1.89";

const REPLY_BUFFER: usize = 8192;
const MAX_POLL: std::time::Duration = std::time::Duration::from_millis(200);
const DRAIN_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const MIN_POLL: std::time::Duration = std::time::Duration::from_millis(1);

/// Byte transport owned by a display session.
pub trait Transport {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn read(&mut self, timeout: std::time::Duration) -> Result<Vec<u8>>;
    fn close(&mut self);
}

/// A connected display that can be handshaken and sent frames.
pub trait Screen {
    fn handshake(&mut self) -> Result<String>;
    fn frame(&mut self, bgra: &[u8]) -> Result<String>;
    fn close(&mut self);
}

/// Consumes bounded, possibly fragmented USB replies without retransmission.
pub fn wait_for_reply(
    transport: &mut dyn Transport,
    clock: &dyn Clock,
    cancel: &dyn Cancellation,
    tokens: &[&str],
    timeout: std::time::Duration,
) -> Result<String> {
    let deadline = clock.now() + timeout;
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        if cancel.cancelled() {
            bail!("Stopping");
        }
        let remaining = deadline.saturating_sub(clock.now());
        if remaining.is_zero() {
            let tail = last_text(&buffer, 256);
            bail!("No {tokens:?} within {timeout:?}; last reply={tail:?}");
        }
        let poll = remaining.min(MAX_POLL).max(MIN_POLL);
        let chunk = transport.read(poll)?;
        if !chunk.is_empty() {
            buffer.extend_from_slice(&chunk);
            if buffer.len() > REPLY_BUFFER {
                let excess = buffer.len() - REPLY_BUFFER;
                buffer.drain(..excess);
            }
            let text = String::from_utf8_lossy(&buffer)
                .replace('\0', "")
                .to_lowercase();
            if tokens
                .iter()
                .any(|token| text.contains(&token.to_lowercase()))
            {
                return Ok(text);
            }
        }
    }
}

fn last_text(buffer: &[u8], limit: usize) -> String {
    let start = buffer.len().saturating_sub(limit);
    String::from_utf8_lossy(&buffer[start..]).replace('\0', "")
}

/// Reads until one poll returns no reply or the quiet-wait budget expires.
pub fn drain(
    transport: &mut dyn Transport,
    clock: &dyn Clock,
    cancel: &dyn Cancellation,
    timeout: std::time::Duration,
) -> Result<()> {
    let deadline = clock.now() + timeout;
    loop {
        if cancel.cancelled() {
            bail!("Stopping");
        }
        let remaining = deadline.saturating_sub(clock.now());
        if remaining.is_zero() {
            bail!("Reply stream did not become quiet");
        }
        let poll = remaining.min(DRAIN_POLL).max(MIN_POLL);
        if transport.read(poll)?.is_empty() {
            return Ok(());
        }
    }
}

/// Only known display commands. No fan, pump, firmware or RGB controls.
pub struct DisplaySession {
    transport: Box<dyn Transport>,
    clock: Arc<dyn Clock>,
    cancel: Arc<dyn Cancellation>,
    identity: String,
}

impl DisplaySession {
    pub fn new(
        transport: Box<dyn Transport>,
        clock: Arc<dyn Clock>,
        cancel: Arc<dyn Cancellation>,
    ) -> Self {
        Self {
            transport,
            clock,
            cancel,
            identity: IDENTITY.to_string(),
        }
    }

    fn command(&mut self, hex: &str) -> Result<()> {
        let mut data = encode::unhex(hex)?;
        data.resize(BLOCK, 0);
        self.transport.write(&data)
    }

    pub fn handshake(&mut self) -> Result<String> {
        drain(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            std::time::Duration::from_secs(1),
        )?;
        self.command("01ef6900000001000000c5d3")?;
        wait_for_reply(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            &[IDENTITY],
            std::time::Duration::from_secs(8),
        )
        .context("handshake identity")?;
        self.command("79ef6900000001")?;
        self.command("96ef6900000001")?;
        wait_for_reply(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            &["media_stop"],
            std::time::Duration::from_secs(8),
        )
        .context("handshake media stop")?;
        drain(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            std::time::Duration::from_secs(1),
        )?;
        Ok(self.identity.clone())
    }

    /// Sends exactly one frame; only one frame is ever in flight.
    pub fn send_frame(&mut self, bgra: &[u8]) -> Result<String> {
        let (header, body) = encode::encode_frame(bgra)?;
        // No old ACK may satisfy this frame.
        drain(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            std::time::Duration::from_secs(1),
        )?;
        self.command("86ef6900000001")?;
        self.transport.write(&[0x2c; BLOCK])?;
        self.transport.write(&header)?;
        self.transport.write(&body)?;
        self.command("cfef6900000001")?;
        wait_for_reply(
            self.transport.as_mut(),
            self.clock.as_ref(),
            self.cancel.as_ref(),
            &FRAME_ACKS,
            std::time::Duration::from_secs(12),
        )
    }
}

impl Screen for DisplaySession {
    fn handshake(&mut self) -> Result<String> {
        Self::handshake(self)
    }

    fn frame(&mut self, bgra: &[u8]) -> Result<String> {
        self.send_frame(bgra)
    }

    fn close(&mut self) {
        self.transport.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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

    type Responder = Box<dyn Fn(&[u8], &mut VecDeque<Vec<u8>>)>;

    struct Replies {
        queue: VecDeque<Vec<u8>>,
        clock: FakeClock,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        respond: Option<Responder>,
    }

    impl Replies {
        fn new(clock: FakeClock, replies: &[&[u8]]) -> Self {
            Self {
                queue: replies.iter().map(|reply| reply.to_vec()).collect(),
                clock,
                writes: Arc::default(),
                respond: None,
            }
        }

        fn written(&self) -> Vec<Vec<u8>> {
            self.writes.lock().expect("writes").clone()
        }
    }

    impl Transport for Replies {
        fn write(&mut self, data: &[u8]) -> Result<()> {
            self.writes.lock().expect("writes").push(data.to_vec());
            if let Some(respond) = self.respond.as_mut() {
                respond(data, &mut self.queue);
            }
            Ok(())
        }

        fn read(&mut self, timeout: Duration) -> Result<Vec<u8>> {
            self.clock.advance(timeout.min(Duration::from_millis(100)));
            Ok(self.queue.pop_front().unwrap_or_default())
        }

        fn close(&mut self) {}
    }

    #[test]
    fn delayed_split_ack_and_bounded_timeout() {
        for token in [
            b"full_png_sucess".as_slice(),
            b"full_png_success".as_slice(),
        ] {
            let clock = FakeClock::new();
            let mut transport = Replies::new(
                clock.clone(),
                &[b"chs_noise", b"", &token[..7], &token[7..]],
            );
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let reply = wait_for_reply(
                &mut transport,
                &clock,
                &cancel,
                &FRAME_ACKS,
                Duration::from_secs(2),
            )
            .expect("split ack");
            assert!(reply.contains(std::str::from_utf8(token).unwrap()));
            assert!(transport.written().is_empty());
        }
        let clock = FakeClock::new();
        let mut transport = Replies::new(clock.clone(), &[b"chs_noise".as_slice(); 100]);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let started = clock.now();
        let error = wait_for_reply(
            &mut transport,
            &clock,
            &cancel,
            &FRAME_ACKS,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(error.to_string().contains("No"));
        assert!(clock.now() - started <= Duration::from_millis(310));
        assert!(transport.written().is_empty());

        let cancel = std::sync::atomic::AtomicBool::new(true);
        assert!(
            wait_for_reply(
                &mut transport,
                &clock,
                &cancel,
                &FRAME_ACKS,
                Duration::from_secs(2)
            )
            .is_err()
        );
    }

    #[test]
    fn handshake_drains_old_ack_and_sends_once() {
        let clock = FakeClock::new();
        let mut transport = Replies::new(clock.clone(), &[b"full_png_sucess", b""]);
        let writes = transport.writes.clone();
        transport.respond = Some(Box::new(|data: &[u8], queue: &mut VecDeque<Vec<u8>>| {
            if data.starts_with(&encode::unhex("01ef69").unwrap()) {
                queue.extend([b"chs_".to_vec(), b"5inch.dev1_rom1.89".to_vec()]);
            }
            if data.starts_with(&encode::unhex("96ef69").unwrap()) {
                queue.extend([b"chs_old".to_vec(), b"".to_vec(), b"media_stop".to_vec()]);
            }
            if data.starts_with(&encode::unhex("cfef69").unwrap()) {
                queue.extend([
                    b"chs_noise".to_vec(),
                    b"".to_vec(),
                    b"full_png_sucess".to_vec(),
                ]);
            }
        }));
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let clock_ref: Arc<dyn Clock> = Arc::new(clock.clone());
        let mut session = DisplaySession::new(Box::new(transport), clock_ref, Arc::new(cancel));
        assert_eq!(
            session.handshake().expect("handshake"),
            "chs_5inch.dev1_rom1.89"
        );
        let ack = session
            .frame(&vec![0u8; encode::FRAME_BYTES])
            .expect("frame");
        assert!(ack.contains("full_png_sucess"));
        let writes = writes.lock().expect("writes").clone();
        assert_eq!(
            writes
                .iter()
                .filter(|data| data.starts_with(&encode::unhex("c8ef69").unwrap()))
                .count(),
            1
        );
        let commands: Vec<u8> = writes
            .iter()
            .filter(|data| data.len() == BLOCK)
            .map(|data| data[0])
            .collect();
        assert_eq!(commands, vec![0x01, 0x79, 0x96, 0x86, 0x2c, 0xc8, 0xcf]);
    }

    #[test]
    fn session_timeout_never_retransmits_or_accepts_generic_success() {
        let clock = FakeClock::new();
        let mut replies: Vec<Vec<u8>> = vec![b"full_png_sucess".to_vec(), b"".to_vec()];
        for _ in 0..60 {
            replies.extend([b"chs_stale".to_vec(), b"success".to_vec(), b"".to_vec()]);
        }
        let refs: Vec<&[u8]> = replies.iter().map(|reply| reply.as_slice()).collect();
        let transport = Replies::new(clock.clone(), &refs);
        let writes = transport.writes.clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let clock_ref: Arc<dyn Clock> = Arc::new(clock.clone());
        let mut session = DisplaySession::new(Box::new(transport), clock_ref, Arc::new(cancel));
        let started = clock.now();
        assert!(session.frame(&vec![0u8; encode::FRAME_BYTES]).is_err());
        assert!(
            clock.now() - started < Duration::from_secs(14),
            "bounded frame timeout"
        );
        assert_eq!(
            writes
                .lock()
                .expect("writes")
                .iter()
                .filter(|data| data.starts_with(&encode::unhex("c8ef69").unwrap()))
                .count(),
            1
        );
    }
}
