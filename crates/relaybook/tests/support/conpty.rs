//! Shared by the PTY harnesses. ConPTY asks for the cursor, ends a cooked
//! line on CR, wraps output in VT, and yields EOF when the pseudoconsole closes.

use std::io::Write;

/// Completes a cooked console read. Unix line input still ends on LF.
pub const ENTER: &str = if cfg!(windows) { "\r" } else { "\n" };

/// Remove CSI, OSC, and any other two-byte escape. Text with no ESC is unchanged.
pub fn strip_vt(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        index = skip_escape(bytes, index);
    }
    String::from_utf8(out)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

fn skip_escape(bytes: &[u8], index: usize) -> usize {
    let Some(next) = bytes.get(index + 1).copied() else {
        return bytes.len();
    };
    match next {
        b'[' => skip_csi(bytes, index + 2),
        b']' => skip_osc(bytes, index + 2),
        _ => index + 2,
    }
}

/// CSI: `ESC [` plus parameter and intermediate bytes, through a final in `0x40..=0x7E`.
fn skip_csi(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && !(0x40..=0x7E).contains(&bytes[index]) {
        index += 1;
    }
    if index < bytes.len() {
        index + 1
    } else {
        index
    }
}

/// OSC: `ESC ]` through BEL or `ESC \`.
fn skip_osc(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return index + 1;
        }
        if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
            return index + 2;
        }
        index += 1;
    }
    index
}

/// Reply once to each `ESC[6n` in the accumulated buffer, including a request
/// split across reads. The product does not emit this on Unix, so there it
/// does nothing.
pub fn answer_cursor_queries(buf: &[u8], answered: &mut usize, writer: &mut dyn Write) {
    let found = buf
        .windows(4)
        .filter(|window| *window == b"\x1b[6n")
        .count();
    while *answered < found {
        let _ = writer.write_all(b"\x1b[1;1R");
        let _ = writer.flush();
        *answered += 1;
    }
}
