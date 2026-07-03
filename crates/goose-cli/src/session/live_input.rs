use std::io::IsTerminal;
use std::os::fd::AsRawFd;

/// Non-blocking stdin line reader used while an agent turn is streaming, so
/// slash commands (/status, /stats, /btw …) work without waiting for the turn
/// to finish. The terminal stays in canonical mode between rustyline prompts,
/// so reads only ever deliver complete lines; partially typed input remains in
/// the tty buffer and flows to the next rustyline prompt untouched.
pub struct LiveStdin {
    fd: i32,
    prev_flags: i32,
    buf: Vec<u8>,
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
        if unsafe { libc::fcntl(fd, libc::F_SETFL, prev_flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        Some(Self {
            fd,
            prev_flags,
            buf: Vec::new(),
        })
    }

    pub fn poll_line(&mut self) -> Option<String> {
        let mut tmp = [0u8; 1024];
        loop {
            let n =
                unsafe { libc::read(self.fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
            if n > 0 {
                self.buf.extend_from_slice(&tmp[..n as usize]);
                if (n as usize) < tmp.len() {
                    break;
                }
            } else {
                break;
            }
        }
        let pos = self.buf.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let line: Vec<u8> = self.buf.drain(..=pos).collect();
        Some(String::from_utf8_lossy(&line).trim().to_string())
    }
}

impl Drop for LiveStdin {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(self.fd, libc::F_SETFL, self.prev_flags);
        }
    }
}
