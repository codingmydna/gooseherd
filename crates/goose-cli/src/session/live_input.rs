//! Live stdin steering while an agent turn is streaming.
//!
//! When active (interactive tty on unix), the terminal is switched to raw mode
//! for the duration of the turn so that a bare Esc interrupts it the same way
//! Ctrl+C does, arrow keys and other CSI escape sequences are swallowed, and
//! complete lines are delivered as steering input / slash commands. Printable
//! characters are echoed so the user can see what they type; the terminal is
//! restored when the reader is dropped. Ctrl+C keeps working because ISIG is
//! left enabled — only ICANON and ECHO are cleared.

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
/// terminal bytes into [`LiveInputEvent`]s, echoing printable edits through the
/// supplied callback. Kept separate from the fd handling so it can be tested
/// without a real terminal.
#[derive(Default)]
struct LineParser {
    buf: Vec<u8>,
    line: String,
    esc_deferred: bool,
    escape_sequence_pending: bool,
}

impl LineParser {
    fn feed(&mut self, bytes: &[u8], echo: &mut dyn FnMut(&str)) -> Vec<LiveInputEvent> {
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
                    let byte = self.buf.remove(0);
                    self.esc_deferred = false;
                    match byte {
                        b'\n' | b'\r' => {
                            let line = std::mem::take(&mut self.line).trim().to_string();
                            echo("\n");
                            events.push(LiveInputEvent::Steer(line));
                        }
                        0x7f | 0x08 => {
                            if self.line.pop().is_some() {
                                echo("\u{8} \u{8}");
                            }
                        }
                        b' '..=b'~' => {
                            let ch = byte as char;
                            self.line.push(ch);
                            echo(ch.encode_utf8(&mut [0u8; 4]));
                        }
                        _ => {}
                    }
                }
            }
        }
        events
    }
}

#[cfg(unix)]
mod imp {
    use super::{LineParser, LiveInputEvent};
    use std::io::{IsTerminal, Write};
    use std::os::fd::AsRawFd;

    pub struct LiveStdin {
        fd: i32,
        prev_flags: i32,
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
            let prev_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if prev_flags < 0 {
                return None;
            }

            let mut prev_termios = unsafe { std::mem::zeroed::<libc::termios>() };
            if unsafe { libc::tcgetattr(fd, &mut prev_termios) } < 0 {
                return None;
            }
            let mut raw = prev_termios;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
                return None;
            }

            if unsafe { libc::fcntl(fd, libc::F_SETFL, prev_flags | libc::O_NONBLOCK) } < 0 {
                let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &prev_termios) };
                return None;
            }

            Some(Self {
                fd,
                prev_flags,
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
            let mut out = std::io::stdout();
            self.parser.feed(&bytes, &mut |s| {
                let _ = out.write_all(s.as_bytes());
                let _ = out.flush();
            })
        }
    }

    impl Drop for LiveStdin {
        fn drop(&mut self) {
            unsafe {
                libc::fcntl(self.fd, libc::F_SETFL, self.prev_flags);
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
    fn printable_edits_are_echoed() {
        let mut parser = LineParser::default();
        let mut echoed = String::new();
        parser.feed(b"hi", &mut |s| echoed.push_str(s));
        assert_eq!(echoed, "hi");
    }
}
