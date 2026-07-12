//! Live stdin steering while an agent turn is streaming.
//!
//! When active (interactive tty on unix), the terminal is switched to raw mode
//! for the duration of the turn so that a bare Esc interrupts it the same way
//! Ctrl+C does, arrow keys and other CSI escape sequences are swallowed, and
//! complete lines are delivered as steering input / slash commands. Typed
//! characters are never echoed at the cursor — that would interleave them with
//! streamed model output. Instead every edit reports the full pending line to
//! the caller, which renders it on an owned status line (see
//! `output::steer_pending_update`). The terminal is restored when the reader
//! is dropped. Ctrl+C keeps working because ISIG is left enabled — only ICANON
//! and ECHO are cleared.

use super::looping::{classify_wait_input, escape_sequence_tail_len, WaitInputClass};

#[derive(Debug, PartialEq, Eq)]
pub enum LiveInputEvent {
    /// A completed line typed while the turn streamed (steering / slash command).
    Steer(String),
    /// A bare Esc keypress — interrupt the current turn.
    Interrupt,
}

/// Whether live steering should be active for this turn. The `GOOSE_LIVE_INPUT`
/// knob is an override to DISABLE (`Some(false)`); otherwise live input is on
/// whenever both stdout and stdin are TTYs.
pub fn should_enable_live_input(
    stdout_is_tty: bool,
    stdin_is_tty: bool,
    disable_override: Option<bool>,
) -> bool {
    if disable_override == Some(false) {
        return false;
    }
    stdout_is_tty && stdin_is_tty
}

/// Byte-stream parser shared by the platform reader and unit tests: it turns raw
/// terminal bytes into [`LiveInputEvent`]s, reporting the full pending line
/// through `on_edit` after every edit so the caller can repaint its input line.
/// Kept separate from the fd handling so it can be tested without a real
/// terminal.
#[derive(Default)]
struct LineParser {
    buf: Vec<u8>,
    line: String,
    esc_deferred: bool,
    escape_sequence_pending: bool,
}

impl LineParser {
    fn feed(&mut self, bytes: &[u8], on_edit: &mut dyn FnMut(&str)) -> Vec<LiveInputEvent> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            if self.escape_sequence_pending {
                let (len, complete) = escape_sequence_tail_len(&self.buf);
                if len == 0 {
                    break;
                }
                self.buf.drain(..len);
                self.escape_sequence_pending = !complete;
                if !complete {
                    break;
                }
                continue;
            }

            if self.buf.is_empty() {
                break;
            }

            match classify_wait_input(&self.buf) {
                WaitInputClass::BareEsc => {
                    // A lone 0x1b may be the head of an as-yet-unread CSI
                    // sequence (arrow key), so defer one round before treating
                    // it as a bare Esc interrupt.
                    if self.buf.len() == 1 && !self.esc_deferred {
                        self.esc_deferred = true;
                        break;
                    }
                    self.buf.drain(..1);
                    self.esc_deferred = false;
                    events.push(LiveInputEvent::Interrupt);
                }
                WaitInputClass::EscapeSequence { len, complete } => {
                    self.buf.drain(..len);
                    self.esc_deferred = false;
                    self.escape_sequence_pending = !complete;
                    if !complete {
                        break;
                    }
                }
                WaitInputClass::Ordinary => {
                    self.esc_deferred = false;
                    match self.buf[0] {
                        b'\n' | b'\r' => {
                            self.buf.remove(0);
                            let line = std::mem::take(&mut self.line).trim().to_string();
                            events.push(LiveInputEvent::Steer(line));
                        }
                        0x7f | 0x08 => {
                            self.buf.remove(0);
                            if self.line.pop().is_some() {
                                on_edit(&self.line);
                            }
                        }
                        b' '..=b'~' => {
                            let ch = self.buf.remove(0) as char;
                            self.line.push(ch);
                            on_edit(&self.line);
                        }
                        0x00..=0x1f => {
                            self.buf.remove(0);
                        }
                        lead => {
                            // Start of a multi-byte UTF-8 sequence (CJK, accents,
                            // emoji). Consume the whole sequence, buffering an
                            // incomplete tail across reads; resync past an invalid
                            // byte rather than dropping the character silently.
                            let expected = utf8_char_len(lead);
                            if self.buf.len() < expected {
                                break;
                            }
                            match std::str::from_utf8(&self.buf[..expected]) {
                                Ok(text) => {
                                    let ch = text.chars().next().unwrap();
                                    self.buf.drain(..expected);
                                    self.line.push(ch);
                                    on_edit(&self.line);
                                }
                                Err(_) => {
                                    self.buf.remove(0);
                                }
                            }
                        }
                    }
                }
            }
        }
        events
    }
}

/// Expected byte length of a UTF-8 sequence from its lead byte. Continuation
/// bytes and invalid leads report 1 so the caller resyncs by a single byte.
fn utf8_char_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

#[cfg(unix)]
mod imp {
    use super::{LineParser, LiveInputEvent};
    use std::io::IsTerminal;
    use std::os::fd::AsRawFd;

    pub struct LiveStdin {
        fd: i32,
        prev_termios: libc::termios,
        parser: LineParser,
    }

    impl LiveStdin {
        pub fn enable() -> Option<Self> {
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                return None;
            }
            let fd = stdin.as_raw_fd();

            let mut prev_termios = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(fd, &mut prev_termios) } < 0 {
                return None;
            }
            // VMIN=0/VTIME=0 makes tty reads return immediately when no input
            // is pending — do NOT reach for O_NONBLOCK here. stdin and stdout
            // usually share one open file description, so setting O_NONBLOCK
            // on fd 0 makes stdout writes fail with EAGAIN under heavy
            // streaming, which panics print! ("os error 35").
            let mut raw = prev_termios;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
                return None;
            }

            Some(Self {
                fd,
                prev_termios,
                parser: LineParser::default(),
            })
        }

        pub fn poll_events(&mut self) -> Vec<LiveInputEvent> {
            let mut tmp = [0u8; 1024];
            let mut bytes = Vec::new();
            loop {
                let n = unsafe {
                    libc::read(self.fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
                };
                if n > 0 {
                    bytes.extend_from_slice(&tmp[..n as usize]);
                    if (n as usize) < tmp.len() {
                        break;
                    }
                } else {
                    break;
                }
            }
            self.parser
                .feed(&bytes, &mut super::super::output::steer_pending_update)
        }
    }

    impl Drop for LiveStdin {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.prev_termios);
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::LiveInputEvent;

    pub struct LiveStdin;

    impl LiveStdin {
        pub fn enable() -> Option<Self> {
            None
        }

        pub fn poll_events(&mut self) -> Vec<LiveInputEvent> {
            Vec::new()
        }
    }
}

pub use imp::LiveStdin;

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut LineParser, bytes: &[u8]) -> Vec<LiveInputEvent> {
        parser.feed(bytes, &mut |_| {})
    }

    #[test]
    fn gating_respects_disable_override_and_ttys() {
        assert!(should_enable_live_input(true, true, None));
        assert!(should_enable_live_input(true, true, Some(true)));
        assert!(!should_enable_live_input(true, true, Some(false)));
        assert!(!should_enable_live_input(false, true, None));
        assert!(!should_enable_live_input(true, false, None));
    }

    #[test]
    fn bare_esc_becomes_interrupt_after_deferral() {
        let mut parser = LineParser::default();
        // A lone Esc byte is held back one round in case it heads a CSI sequence.
        assert!(feed(&mut parser, &[0x1b]).is_empty());
        assert_eq!(feed(&mut parser, &[]), vec![LiveInputEvent::Interrupt]);
    }

    #[test]
    fn arrow_key_csi_is_swallowed_not_interrupted() {
        let mut parser = LineParser::default();
        assert!(feed(&mut parser, &[0x1b, b'[', b'A']).is_empty());
        // Nothing pending afterwards: a later line still parses cleanly.
        assert_eq!(
            feed(&mut parser, b"go\n"),
            vec![LiveInputEvent::Steer("go".to_string())]
        );
    }

    #[test]
    fn escape_sequence_split_across_reads_is_swallowed() {
        let mut parser = LineParser::default();
        assert!(feed(&mut parser, &[0x1b]).is_empty());
        assert!(feed(&mut parser, b"[B").is_empty());
    }

    #[test]
    fn complete_line_becomes_steer_event() {
        let mut parser = LineParser::default();
        assert!(feed(&mut parser, b"hel").is_empty());
        assert_eq!(
            feed(&mut parser, b"lo\n"),
            vec![LiveInputEvent::Steer("hello".to_string())]
        );
    }

    #[test]
    fn backspace_edits_the_pending_line() {
        let mut parser = LineParser::default();
        feed(&mut parser, b"cat");
        feed(&mut parser, &[0x7f]);
        assert_eq!(
            feed(&mut parser, b"n\n"),
            vec![LiveInputEvent::Steer("can".to_string())]
        );
    }

    #[test]
    fn edits_report_the_full_pending_line() {
        let mut parser = LineParser::default();
        let mut states: Vec<String> = Vec::new();
        parser.feed(b"hi", &mut |line| states.push(line.to_string()));
        assert_eq!(states, vec!["h".to_string(), "hi".to_string()]);
        parser.feed(&[0x7f], &mut |line| states.push(line.to_string()));
        assert_eq!(states.last().map(String::as_str), Some("h"));
    }

    #[test]
    fn alt_key_chord_is_swallowed_not_interrupted() {
        // Alt-x arrives as ESC + 'x' in one read; it must not interrupt the turn.
        let mut parser = LineParser::default();
        assert!(feed(&mut parser, &[0x1b, b'x']).is_empty());
        // Alt+Backspace (ESC + DEL) likewise swallowed, and a later line parses.
        assert!(feed(&mut parser, &[0x1b, 0x7f]).is_empty());
        assert_eq!(
            feed(&mut parser, b"go\n"),
            vec![LiveInputEvent::Steer("go".to_string())]
        );
    }

    #[test]
    fn paste_containing_embedded_escape_does_not_interrupt() {
        let mut parser = LineParser::default();
        let events = feed(&mut parser, b"abc\x1bdef\n");
        // No interrupt event; the line is delivered (the ESC + next byte is
        // swallowed as an Alt chord).
        assert_eq!(events, vec![LiveInputEvent::Steer("abcef".to_string())]);
    }

    #[test]
    fn korean_utf8_split_across_reads_delivers_full_line() {
        let mut parser = LineParser::default();
        let text = "먼저 테스트 고쳐";
        let bytes = text.as_bytes();
        let split = 5; // slice mid multi-byte sequence
        assert!(feed(&mut parser, &bytes[..split]).is_empty());
        let mut rest = bytes[split..].to_vec();
        rest.push(b'\n');
        assert_eq!(
            feed(&mut parser, &rest),
            vec![LiveInputEvent::Steer(text.to_string())]
        );
    }

    #[test]
    fn invalid_utf8_byte_is_skipped_without_panic() {
        let mut parser = LineParser::default();
        // A stray continuation byte 0x80 is resynced past; the ASCII survives.
        assert_eq!(
            feed(&mut parser, &[0x80, b'o', b'k', b'\n']),
            vec![LiveInputEvent::Steer("ok".to_string())]
        );
    }
}
