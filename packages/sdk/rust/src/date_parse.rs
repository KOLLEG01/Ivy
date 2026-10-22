// Date grammar adapted from V8's dateparser.{h,cc} and dateparser-inl.h,
// shipped in Node v24.18.0. Copyright 2011 the V8 project authors.
// See V8-LICENSE for the BSD license and redistribution terms.
use crate::JsString;

const LIMIT: i64 = 8_640_000_000_000_000;
const DAY: i64 = 86_400_000;

#[derive(Clone, Copy, PartialEq)]
enum Token {
    End,
    Unknown,
    Space,
    Symbol(u16),
    Number(i64, usize),
    Month(i64),
    Zone(i64, usize),
    AmPm(i64),
    Separator,
    Word,
}
impl Token {
    fn number(self) -> Option<i64> {
        if let Self::Number(n, _) = self {
            Some(n)
        } else {
            None
        }
    }
    fn fixed(self, length: usize, low: i64, high: i64) -> bool {
        matches!(self, Self::Number(n,l) if l == length && (low..=high).contains(&n))
    }
    fn sign(self) -> Option<i64> {
        match self {
            Self::Symbol(43) => Some(1),
            Self::Symbol(45) => Some(-1),
            _ => None,
        }
    }
    fn millis(self) -> i64 {
        let Self::Number(n, len) = self else {
            unreachable!()
        };
        match len {
            1 => n * 100,
            2 => n * 10,
            3 => n,
            _ => n / 10_i64.pow((len.min(9) - 3) as u32),
        }
    }
}
fn space(c: u16) -> bool {
    matches!(
        c,
        9 | 11 | 12 | 32 | 160 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}
struct Scanner<'a> {
    value: &'a [u16],
    at: usize,
    peek: Token,
}
impl<'a> Scanner<'a> {
    fn new(value: &'a [u16]) -> Self {
        let mut s = Self {
            value,
            at: 0,
            peek: Token::End,
        };
        s.peek = s.scan();
        s
    }
    fn char(&self) -> u16 {
        self.value.get(self.at).copied().unwrap_or(0)
    }
    fn next(&mut self) -> Token {
        let t = self.peek;
        self.peek = self.scan();
        t
    }
    fn skip(&mut self, c: u16) -> bool {
        if self.peek == Token::Symbol(c) {
            self.next();
            true
        } else {
            false
        }
    }
    fn scan(&mut self) -> Token {
        let c = self.char();
        if c == 0 {
            return Token::End;
        }
        if (48..=57).contains(&c) {
            let start = self.at;
            let mut n = 0;
            let mut significant = 0;
            while self.char() == 48 {
                self.at += 1;
            }
            while (48..=57).contains(&self.char()) {
                if significant < 9 {
                    n = n * 10 + i64::from(self.char() - 48);
                }
                significant += 1;
                self.at += 1;
            }
            return Token::Number(n, self.at - start);
        }
        if matches!(c, 58 | 45 | 43 | 46 | 41) {
            self.at += 1;
            return Token::Symbol(c);
        }
        if c >= 65 && !space(c) {
            let start = self.at;
            let mut prefix = [0u16; 3];
            while self.char() >= 65 && !space(self.char()) {
                let index = self.at - start;
                if index < 3 {
                    let c = self.char();
                    prefix[index] = if (65..=90).contains(&c) { c + 32 } else { c };
                }
                self.at += 1;
            }
            let len = self.at - start;
            for (index, month) in [
                b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct",
                b"nov", b"dec",
            ]
            .iter()
            .enumerate()
            {
                if prefix == month.map(u16::from) {
                    return Token::Month(index as i64 + 1);
                }
            }
            if len > 3 {
                return Token::Word;
            }
            let p = prefix.map(|c| u8::try_from(c).unwrap_or(255));
            match &p {
                b"am\0" => Token::AmPm(0),
                b"pm\0" => Token::AmPm(12),
                b"t\0\0" => Token::Separator,
                b"ut\0" | b"utc" | b"z\0\0" | b"gmt" => Token::Zone(0, len),
                b"cdt" | b"est" => Token::Zone(-5, len),
                b"cst" | b"mdt" => Token::Zone(-6, len),
                b"edt" => Token::Zone(-4, len),
                b"mst" | b"pdt" => Token::Zone(-7, len),
                b"pst" => Token::Zone(-8, len),
                _ => Token::Word,
            }
        } else if space(c) || matches!(c, 10 | 13 | 0x2028 | 0x2029) {
            self.at += 1;
            Token::Space
        } else if c == 40 {
            let mut depth = 0;
            loop {
                let c = self.char();
                if c == 40 {
                    depth += 1;
                } else if c == 41 {
                    depth -= 1;
                }
                self.at += 1;
                if depth == 0 || self.char() == 0 {
                    break;
                }
            }
            Token::Unknown
        } else {
            self.at += 1;
            Token::Unknown
        }
    }
}
#[derive(Default)]
struct Parts {
    day: Vec<i64>,
    time: Vec<i64>,
    month: Option<i64>,
    ampm: Option<i64>,
    sign: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    iso: bool,
}
impl Parts {
    fn day(&mut self, n: i64) -> Option<()> {
        if self.day.len() == 3 {
            return None;
        }
        self.day.push(n);
        Some(())
    }
    fn time(&mut self, n: i64) -> Option<()> {
        if self.time.len() == 4 {
            return None;
        }
        self.time.push(n);
        Some(())
    }
    fn final_time(&mut self, n: i64) {
        if self.time(n).is_some() {
            self.time.resize(4, 0);
        }
    }
    fn expects_time(&self, n: i64) -> bool {
        match self.time.len() {
            1 | 2 => (0..60).contains(&n),
            3 => (0..1000).contains(&n),
            _ => false,
        }
    }
    fn zone(&mut self, h: i64) {
        self.sign = Some(if h < 0 { -1 } else { 1 });
        self.hour = Some(h.abs());
        self.minute = Some(0);
    }
    fn compose(mut self) -> Option<(i64, Option<i64>)> {
        if self.day.is_empty() {
            return None;
        }
        self.day.resize(3, 1);
        let [a, b, c] = self.day[..] else {
            unreachable!()
        };
        let (mut year, month, day) = if let Some(month) = self.month {
            if !(1..=31).contains(&a) {
                (a, month, b)
            } else {
                (b, month, a)
            }
        } else if self.iso || !(1..=31).contains(&a) {
            (a, b, c)
        } else {
            (c, a, b)
        };
        if !self.iso {
            if (0..50).contains(&year) {
                year += 2000;
            } else if (50..100).contains(&year) {
                year += 1900;
            }
        }
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year.abs() > 1_000_000 {
            return None;
        }
        self.time.resize(4, 0);
        let mut h = self.time[0];
        let m = self.time[1];
        let s = self.time[2];
        let ms = self.time[3];
        if let Some(offset) = self.ampm {
            if !(0..=12).contains(&h) {
                return None;
            }
            h = h % 12 + offset;
        }
        if !(h == 24 && m == 0 && s == 0 && ms == 0)
            && (!(0..24).contains(&h)
                || !(0..60).contains(&m)
                || !(0..60).contains(&s)
                || !(0..1000).contains(&ms))
        {
            return None;
        }
        let y = year - i64::from(month <= 2);
        let era = y.div_euclid(400);
        let y = y - era * 400;
        let mo = month + if month > 2 { -3 } else { 9 };
        let days = era * 146097 + y * 365 + y / 4 - y / 100 + (153 * mo + 2) / 5 + day - 1 - 719468;
        let wall = days * DAY + h * 3_600_000 + m * 60_000 + s * 1000 + ms;
        let offset = if let Some(sign) = self.sign {
            // V8's unsigned composition followed by its 31-bit Smi bound.
            let seconds = (self.hour.unwrap_or(0) as u32)
                .wrapping_mul(3600)
                .wrapping_add((self.minute.unwrap_or(0) as u32).wrapping_mul(60));
            if seconds > i32::MAX as u32 {
                return None;
            }
            Some(i64::from(seconds) * sign * 1000)
        } else {
            None
        };
        Some((wall, offset))
    }
}
fn iso(s: &mut Scanner<'_>, p: &mut Parts) -> Option<Token> {
    if let Some(sign) = s.peek.sign() {
        let token = s.next();
        if !s.peek.fixed(6, 0, 999999) {
            return Some(token);
        }
        let year = s.next().number()?;
        if sign < 0 && year == 0 {
            return Some(token);
        }
        p.day(sign * year)?;
    } else if s.peek.fixed(4, 0, 9999) {
        p.day(s.next().number()?)?;
    } else {
        return Some(s.next());
    }
    if s.skip(45) {
        if !s.peek.fixed(2, 1, 12) {
            return Some(s.next());
        }
        p.day(s.next().number()?)?;
        if s.skip(45) {
            if !s.peek.fixed(2, 1, 31) {
                return Some(s.next());
            }
            p.day(s.next().number()?)?;
        }
    }
    if s.peek != Token::Separator {
        if s.peek != Token::End {
            return Some(s.next());
        }
    } else {
        s.next();
        if !s.peek.fixed(2, 0, 24) {
            return None;
        }
        let h = s.next().number()?;
        p.time(h)?;
        if !s.skip(58) || !s.peek.fixed(2, 0, if h == 24 { 0 } else { 59 }) {
            return None;
        }
        p.time(s.next().number()?)?;
        if s.skip(58) {
            if !s.peek.fixed(2, 0, if h == 24 { 0 } else { 59 }) {
                return None;
            }
            p.time(s.next().number()?)?;
            if s.skip(46) {
                let n = s.peek.number()?;
                if h == 24 && n > 0 {
                    return None;
                }
                p.time(s.next().millis())?;
            }
        }
        if s.peek == Token::Zone(0, 1) {
            s.next();
            p.zone(0);
        } else if let Some(sign) = s.peek.sign() {
            s.next();
            p.sign = Some(sign);
            if s.peek.fixed(4, 0, 9999) {
                let n = s.next().number()?;
                if n / 100 > 23 || n % 100 > 59 {
                    return None;
                }
                p.hour = Some(n / 100);
                p.minute = Some(n % 100);
            } else {
                if !s.peek.fixed(2, 0, 23) {
                    return None;
                }
                p.hour = s.next().number();
                if !s.skip(58) || !s.peek.fixed(2, 0, 59) {
                    return None;
                }
                p.minute = s.next().number();
            }
        }
        if s.peek != Token::End {
            return None;
        }
    }
    if p.hour.is_none() && p.time.is_empty() {
        p.zone(0);
    }
    p.iso = true;
    Some(Token::End)
}
fn components(value: &JsString) -> Option<(i64, Option<i64>)> {
    let mut s = Scanner::new(&value.0);
    let mut p = Parts::default();
    let mut token = iso(&mut s, &mut p)?;
    let mut read_number = !p.day.is_empty();
    while token != Token::End {
        match token {
            Token::Number(n, _) => {
                read_number = true;
                if s.skip(58) {
                    if s.skip(58) {
                        if !p.time.is_empty() {
                            return None;
                        }
                        p.time(n)?;
                        p.time(0)?;
                    } else {
                        p.time(n)?;
                        s.skip(46);
                    }
                } else if s.skip(46) && p.expects_time(n) {
                    p.time(n)?;
                    s.peek.number()?;
                    p.final_time(s.next().millis());
                } else if p.hour.is_some() && p.minute.is_none() && (0..60).contains(&n) {
                    p.minute = Some(n);
                } else if p.expects_time(n) {
                    p.final_time(n);
                    if !matches!(s.peek, Token::End | Token::Space | Token::Zone(0, 1))
                        && s.peek.sign().is_none()
                    {
                        return None;
                    }
                } else {
                    p.day(n)?;
                    s.skip(45);
                }
            }
            Token::Month(m) => {
                p.month = Some(m);
                s.skip(45);
            }
            Token::AmPm(offset) if !p.time.is_empty() => p.ampm = Some(offset),
            Token::Zone(offset, _) if read_number => p.zone(offset),
            Token::AmPm(_) | Token::Zone(_, _) | Token::Word | Token::Separator => {
                if read_number || s.peek.number().is_some() {
                    return None;
                }
            }
            t if t.sign().is_some()
                && ((p.hour == Some(0) && p.minute == Some(0)) || !p.time.is_empty()) =>
            {
                p.sign = t.sign();
                let (n, length) = if let Token::Number(n, l) = s.peek {
                    s.next();
                    (n, l)
                } else {
                    (0, 0)
                };
                read_number = true;
                if s.peek == Token::Symbol(58) {
                    p.hour = Some(n);
                    p.minute = None;
                } else if length == 1 || length == 2 {
                    p.hour = Some(n);
                    p.minute = Some(0);
                } else if length == 3 || length == 4 {
                    p.hour = Some(n / 100);
                    p.minute = Some(n % 100);
                } else {
                    return None;
                }
            }
            t if read_number && (t.sign().is_some() || t == Token::Symbol(41)) => return None,
            _ => {}
        }
        token = s.next();
    }
    p.compose()
}

/// Parse the original JavaScript string grammar with the caller's local-time resolver.
/// The resolver receives wall-clock milliseconds and returns the UTC offset in milliseconds.
pub fn parse_with_local_offset(
    value: &JsString,
    local_offset: impl FnOnce(i64) -> Option<i64>,
) -> f64 {
    let Some((wall, offset)) = components(value) else {
        return f64::NAN;
    };
    let offset = match offset {
        Some(offset) => offset,
        None => {
            if wall.abs() > LIMIT + 30 * DAY {
                return f64::NAN;
            }
            let Some(offset) = local_offset(wall) else {
                return f64::NAN;
            };
            offset
        }
    };
    let Some(utc) = wall.checked_sub(offset) else {
        return f64::NAN;
    };
    if utc.unsigned_abs() > LIMIT as u64 {
        f64::NAN
    } else {
        utc as f64
    }
}

pub(crate) fn zone_offset(wall: i64, zone: &tz::TimeZone) -> Option<i64> {
    let date = tz::UtcDateTime::from_timespec(wall.div_euclid(1000), 0).ok()?;
    let found = tz::DateTime::find(
        date.year(),
        date.month(),
        date.month_day(),
        date.hour(),
        date.minute(),
        date.second(),
        0,
        zone.as_ref(),
    )
    .ok()?;
    // Use the earlier occurrence in a fold and the pre-transition offset in a gap.
    Some(i64::from(found.earliest()?.local_time_type().ut_offset()) * 1000)
}
pub(crate) fn named_zone(name: &str) -> Option<tz::TimeZone> {
    jiff_tzdb::get(name)
        .filter(|(exact, _)| *exact == name)
        .and_then(|(_, data)| tz::TimeZone::from_tz_data(data).ok())
}
fn runtime_zone(name: &str) -> tz::TimeZone {
    // ICU's retained three-letter zone aliases are not date-string abbreviations.
    let name = match name {
        "ACT" => "Australia/Darwin",
        "AET" => "Australia/Sydney",
        "AGT" => "America/Buenos_Aires",
        "ART" => "Africa/Cairo",
        "AST" => "America/Anchorage",
        "BET" => "America/Sao_Paulo",
        "BST" => "Asia/Dhaka",
        "CAT" => "Africa/Maputo",
        "CNT" => "America/St_Johns",
        "CST" => "America/Chicago",
        "CTT" => "Asia/Shanghai",
        "EAT" => "Africa/Nairobi",
        "ECT" => "Europe/Paris",
        "IET" => "America/Indianapolis",
        "IST" => "Asia/Calcutta",
        "JST" => "Asia/Tokyo",
        "MIT" => "Pacific/Apia",
        "NET" => "Asia/Yerevan",
        "NST" => "Pacific/Auckland",
        "PLT" => "Asia/Karachi",
        "PNT" => "America/Phoenix",
        "PRT" => "America/Puerto_Rico",
        "PST" => "America/Los_Angeles",
        "SST" => "Pacific/Guadalcanal",
        "VST" => "Asia/Saigon",
        _ => name,
    };
    named_zone(name)
        .or_else(|| custom_gmt(name).and_then(|offset| tz::TimeZone::fixed(offset).ok()))
        .unwrap_or_else(tz::TimeZone::utc)
}
fn custom_gmt(name: &str) -> Option<i32> {
    let bytes = name.as_bytes();
    if bytes.len() < 5 || !bytes[..3].eq_ignore_ascii_case(b"GMT") {
        return None;
    }
    let sign = match bytes[3] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let fields: Vec<_> = name[4..].split(':').collect();
    let number = |s: &str| -> Option<i32> {
        if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (h, m, s) = match fields.as_slice() {
        [digits] => {
            let n = number(digits)?;
            match digits.len() {
                1 | 2 => (n, 0, 0),
                3 | 4 => (n / 100, n % 100, 0),
                5 | 6 => (n / 10000, n / 100 % 100, n % 100),
                _ => return None,
            }
        }
        [h, m] if (1..=2).contains(&h.len()) && m.len() == 2 => (number(h)?, number(m)?, 0),
        [h, m, s] if (1..=2).contains(&h.len()) && m.len() == 2 && s.len() == 2 => {
            (number(h)?, number(m)?, number(s)?)
        }
        _ => return None,
    };
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + s))
}
/// Date.parse with an explicit local IANA zone, without changing process state.
/// Invalid dates or an unavailable zone return NaN; explicit-offset dates need no zone.
pub fn parse_date_in_zone(value: &JsString, zone: &str) -> f64 {
    parse_with_local_offset(value, |wall| zone_offset(wall, &named_zone(zone)?))
}
/// JavaScript Date.parse(string), using TZ or the system IANA zone for local forms.
pub fn parse_date(value: &JsString) -> f64 {
    parse_with_local_offset(value, |wall| {
        with_runtime_zone(|zone| zone_offset(wall, zone))
    })
}

pub(crate) fn with_runtime_zone<T>(action: impl FnOnce(&tz::TimeZone) -> Option<T>) -> Option<T> {
    use std::sync::{Mutex, OnceLock};
    static SYSTEM: OnceLock<String> = OnceLock::new();
    static CACHE: OnceLock<Mutex<Option<(String, tz::TimeZone)>>> = OnceLock::new();
    let name = std::env::var("TZ")
        .ok()
        .filter(|name| !cfg!(windows) || !name.is_empty())
        .unwrap_or_else(|| {
            SYSTEM
                .get_or_init(|| iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()))
                .clone()
        });
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().ok()?;
    if cache.as_ref().is_none_or(|(cached, _)| cached != &name) {
        // ICU uses UTC for an unrecognized TZ identifier.
        let zone = runtime_zone(&name);
        *cache = Some((name, zone));
    }
    action(&cache.as_ref()?.1)
}
