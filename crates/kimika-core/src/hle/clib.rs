//! the C library functions SceLibKernel exports as sceClib, which newlib
//! builds its memset and friends on, and their printf.

use std::io::Write;

use super::{Call, Function};
use crate::memory::Memory;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceClibMemset", memset),
    ("sceClibMemcpy", memcpy),
    ("sceClibMemmove", memcpy),
    ("sceClibMemcmp", memcmp),
    ("sceClibStrlen", strlen),
    ("sceClibStrnlen", strnlen),
    ("sceClibStrcmp", strcmp),
    ("sceClibStrncmp", strncmp),
    ("sceClibStrncpy", strncpy),
    ("sceClibPrintf", printf),
    ("sceClibSnprintf", snprintf),
    ("sceClibVsnprintf", vsnprintf),
];

/// sceClibMemset(destination, byte, count) -> destination
fn memset(call: &mut Call) {
    let (destination, byte, count) = (call.u32(), call.u32(), call.u32());
    call.memory().fill(destination, byte as u8, count);
    call.ret(destination);
}

/// sceClibMemcpy and sceClibMemmove(destination, source, count) ->
/// destination, both safe across an overlap.
fn memcpy(call: &mut Call) {
    let (destination, source, count) = (call.u32(), call.u32(), call.u32());
    call.memory().copy(destination, source, count);
    call.ret(destination);
}

/// sceClibMemcmp(a, b, count)
fn memcmp(call: &mut Call) {
    let (a, b, count) = (call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let order = (0..count)
        .map(|i| (memory.read8(a.wrapping_add(i)), memory.read8(b.wrapping_add(i))))
        .find(|(x, y)| x != y)
        .map_or(0, |(x, y)| x as i32 - y as i32);
    call.ret(order);
}

/// the length of the string at address, at most max.
fn length(memory: &Memory, address: u32, max: u32) -> u32 {
    (0..max).find(|&i| memory.read8(address.wrapping_add(i)) == 0).unwrap_or(max)
}

/// sceClibStrlen(string)
fn strlen(call: &mut Call) {
    let string = call.u32();
    call.ret(length(call.memory(), string, u32::MAX));
}

/// sceClibStrnlen(string, max)
fn strnlen(call: &mut Call) {
    let (string, max) = (call.u32(), call.u32());
    call.ret(length(call.memory(), string, max));
}

/// compares strings a and b, at most max bytes.
fn compare(memory: &Memory, a: u32, b: u32, max: u32) -> i32 {
    for i in 0..max {
        let (x, y) = (memory.read8(a.wrapping_add(i)), memory.read8(b.wrapping_add(i)));
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

/// sceClibStrcmp(a, b)
fn strcmp(call: &mut Call) {
    let (a, b) = (call.u32(), call.u32());
    call.ret(compare(call.memory(), a, b, u32::MAX));
}

/// sceClibStrncmp(a, b, max)
fn strncmp(call: &mut Call) {
    let (a, b, max) = (call.u32(), call.u32(), call.u32());
    call.ret(compare(call.memory(), a, b, max));
}

/// sceClibStrncpy(destination, source, count) -> destination, padded with
/// zeros as strncpy does.
fn strncpy(call: &mut Call) {
    let (destination, source, count) = (call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let copied = length(memory, source, count);
    memory.copy(destination, source, copied);
    memory.fill(destination.wrapping_add(copied), 0, count - copied);
    call.ret(destination);
}

/// sceClibPrintf(format, ...) to the host's standard output.
fn printf(call: &mut Call) {
    let format = call.u32();
    let text = formatted(call.memory(), format, call);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
    call.ret(text.len() as u32);
}

/// sceClibSnprintf(buffer, size, format, ...)
fn snprintf(call: &mut Call) {
    let (buffer, size, format) = (call.u32(), call.u32(), call.u32());
    let text = formatted(call.memory(), format, call);
    store(call.memory(), buffer, size, &text);
    call.ret(text.len() as u32);
}

/// sceClibVsnprintf(buffer, size, format, arguments)
fn vsnprintf(call: &mut Call) {
    let (buffer, size, format, list) = (call.u32(), call.u32(), call.u32(), call.u32());
    let memory = call.memory();
    let text = formatted(memory, format, &mut VaList { memory, at: list });
    store(memory, buffer, size, &text);
    call.ret(text.len() as u32);
}

/// text into a buffer of size bytes, cut short to leave room for the NUL.
fn store(memory: &Memory, buffer: u32, size: u32, text: &str) {
    if size == 0 {
        return;
    }
    let kept = text.len().min(size as usize - 1);
    memory.write_bytes(buffer, &text.as_bytes()[..kept]);
    memory.write8(buffer.wrapping_add(kept as u32), 0);
}

/// where printf's arguments come from: the call itself, as variadic
/// arguments are passed in core registers and on the stack, doubles in even
/// pairs, or a va_list, which points at them in memory.
pub trait Arguments {
    fn word(&mut self) -> u32;
    fn double_word(&mut self) -> u64;
}

impl Arguments for Call<'_> {
    fn word(&mut self) -> u32 {
        self.u32()
    }
    fn double_word(&mut self) -> u64 {
        self.u64()
    }
}

struct VaList<'a> {
    memory: &'a Memory,
    at: u32,
}

impl Arguments for VaList<'_> {
    fn word(&mut self) -> u32 {
        let value = self.memory.read32(self.at);
        self.at = self.at.wrapping_add(4);
        value
    }
    fn double_word(&mut self) -> u64 {
        self.at = self.at.next_multiple_of(8);
        self.word() as u64 | (self.word() as u64) << 32
    }
}

/// what printf makes of the format string at address and the arguments:
/// flags, width, precision and length, and the conversions d i u o x X c s
/// p f F e E g G and %.
pub fn formatted(memory: &Memory, address: u32, arguments: &mut impl Arguments) -> String {
    let format = memory.read_cstring(address, 4096).unwrap_or_default();
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let (mut left, mut zero, mut plus, mut space, mut alternate) = (false, false, false, false, false);
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' => space = true,
                '#' => alternate = true,
                _ => break,
            }
            chars.next();
        }
        let number = |chars: &mut std::iter::Peekable<std::str::Chars>, arguments: &mut dyn FnMut() -> u32| {
            if chars.peek() == Some(&'*') {
                chars.next();
                return Some(arguments() as i32 as i64);
            }
            let mut value = None;
            while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
                value = Some(value.unwrap_or(0) * 10 + digit as i64);
                chars.next();
            }
            value
        };
        let mut width = number(&mut chars, &mut || arguments.word()).unwrap_or(0);
        if width < 0 {
            left = true;
            width = -width;
        }
        let precision = if chars.peek() == Some(&'.') {
            chars.next();
            Some(number(&mut chars, &mut || arguments.word()).unwrap_or(0).max(0) as usize)
        } else {
            None
        };
        let mut long = 0;
        while let Some(&length) = chars.peek() {
            match length {
                'l' => long += 1,
                'h' | 'z' | 'j' | 't' | 'L' => {}
                _ => break,
            }
            chars.next();
        }
        let Some(conversion) = chars.next() else { break };
        let signed = |value: i64| {
            let sign = if value < 0 {
                "-"
            } else if plus {
                "+"
            } else if space {
                " "
            } else {
                ""
            };
            (sign.to_owned(), value.unsigned_abs().to_string())
        };
        let integer =
            |arguments: &mut dyn Arguments| if long >= 2 { arguments.double_word() } else { arguments.word() as u64 };
        let (prefix, mut body) = match conversion {
            '%' => {
                out.push('%');
                continue;
            }
            'd' | 'i' => {
                let value = integer(arguments);
                signed(if long >= 2 { value as i64 } else { value as u32 as i32 as i64 })
            }
            'u' => (String::new(), integer(arguments).to_string()),
            'o' => (if alternate { "0" } else { "" }.to_owned(), format!("{:o}", integer(arguments))),
            'x' => (if alternate { "0x" } else { "" }.to_owned(), format!("{:x}", integer(arguments))),
            'X' => (if alternate { "0X" } else { "" }.to_owned(), format!("{:X}", integer(arguments))),
            'p' => ("0x".to_owned(), format!("{:x}", arguments.word())),
            'c' => (String::new(), (arguments.word() as u8 as char).to_string()),
            's' => {
                let text = match arguments.word() {
                    0 => "(null)".to_owned(),
                    pointer => memory.read_cstring(pointer, precision.unwrap_or(4096)).unwrap_or_default(),
                };
                let text: String = text.chars().take(precision.unwrap_or(usize::MAX)).collect();
                (String::new(), text)
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                let value = f64::from_bits(arguments.double_word());
                let digits = precision.unwrap_or(6);
                let text = match conversion {
                    'f' | 'F' => format!("{:.*}", digits, value.abs()),
                    'e' | 'E' => format!("{:.*e}", digits, value.abs()),
                    _ => format!("{}", value.abs()),
                };
                let sign = if value.is_sign_negative() {
                    "-"
                } else if plus {
                    "+"
                } else {
                    ""
                };
                (sign.to_owned(), if conversion.is_uppercase() { text.to_uppercase() } else { text })
            }
            other => {
                out.push('%');
                out.push(other);
                continue;
            }
        };
        // a precision on an integer is its fewest digits
        if let (Some(digits), true) = (precision, "diuoxX".contains(conversion)) {
            if body.len() < digits {
                body = "0".repeat(digits - body.len()) + &body;
            }
        }
        let length = prefix.len() + body.len();
        let pad = (width as usize).saturating_sub(length);
        if left {
            out += &prefix;
            out += &body;
            out += &" ".repeat(pad);
        } else if zero && precision.is_none() && conversion != 's' && conversion != 'c' {
            out += &prefix;
            out += &"0".repeat(pad);
            out += &body;
        } else {
            out += &" ".repeat(pad);
            out += &prefix;
            out += &body;
        }
    }
    out
}
