//! Bounded RE2JS-compatible boolean pattern matching over lossless JS strings.
//! Syntax/tables follow RE2JS 2.8.6 (MIT; see re2-LICENSE.txt).
//! A Thompson machine visits each instruction at most once per input position.
use crate::{IvyError, IvyResult, JsString};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

#[derive(Deserialize)]
struct Tables {
    groups: HashMap<String, [Vec<u32>; 2]>,
    folds: Vec<[u32; 2]>,
}
fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        serde_json::from_str(include_str!("re2-data.json")).expect("generated RE2 tables")
    })
}
fn syntax() -> IvyError {
    IvyError::new(
        "registry_invalid",
        "Schema pattern is outside the supported RE2 syntax.",
    )
}
fn points(text: &JsString) -> Vec<u32> {
    char::decode_utf16(text.0.iter().copied())
        .map(|c| c.map_or_else(|e| u32::from(e.unpaired_surrogate()), u32::from))
        .collect()
}
#[derive(Clone, Copy, Default)]
struct Flags {
    fold: bool,
    multiline: bool,
    dotall: bool,
}
#[derive(Clone, Copy)]
enum Assertion {
    Start,
    End,
    LineStart,
    LineEnd,
    Word,
    NotWord,
}
enum Node {
    Empty,
    Class(Arc<[u32]>),
    Assert(Assertion),
    Concat(Vec<usize>),
    Alt(Vec<usize>),
    Repeat {
        child: usize,
        min: usize,
        max: Option<usize>,
    },
}
struct Frame {
    sequence: Vec<usize>,
    alternatives: Vec<usize>,
    flags: Flags,
    last_repeat: bool,
}
impl Frame {
    fn new(flags: Flags) -> Self {
        Self {
            sequence: Vec::new(),
            alternatives: Vec::new(),
            flags,
            last_repeat: false,
        }
    }
}
struct Parser {
    input: Vec<u32>,
    at: usize,
    flags: Flags,
    nodes: Vec<Node>,
    factors: Vec<usize>,
    frames: Vec<Frame>,
    names: HashSet<String>,
}
impl Parser {
    fn node(&mut self, node: Node, factor: usize) -> usize {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.factors.push(factor);
        id
    }
    fn push(&mut self, node: Node, factor: usize) {
        let id = self.node(node, factor);
        self.frames.last_mut().unwrap().sequence.push(id);
    }
    fn literal(&mut self, point: u32) {
        let mut ranges = vec![(point, point)];
        if self.flags.fold {
            fold(&mut ranges);
        }
        self.push(Node::Class(normalize(ranges)), 1);
    }
    fn starts(&self, text: &str) -> bool {
        self.input[self.at..]
            .iter()
            .copied()
            .zip(text.bytes().map(u32::from))
            .filter(|(a, b)| a == b)
            .count()
            == text.len()
            && self.input.len() - self.at >= text.len()
    }
    fn take(&mut self) -> IvyResult<u32> {
        let c = *self.input.get(self.at).ok_or_else(syntax)?;
        self.at += 1;
        Ok(c)
    }
    fn finish_sequence(&mut self) -> usize {
        let sequence = std::mem::take(&mut self.frames.last_mut().unwrap().sequence);
        if sequence.len() == 1 {
            return sequence[0];
        }
        let factor = sequence
            .iter()
            .map(|id| self.factors[*id])
            .max()
            .unwrap_or(1);
        self.node(
            if sequence.is_empty() {
                Node::Empty
            } else {
                Node::Concat(sequence)
            },
            factor,
        )
    }
    fn finish_frame(&mut self) -> usize {
        let sequence = self.finish_sequence();
        let mut alternatives = std::mem::take(&mut self.frames.last_mut().unwrap().alternatives);
        if alternatives.is_empty() {
            return sequence;
        }
        alternatives.push(sequence);
        let factor = alternatives
            .iter()
            .map(|id| self.factors[*id])
            .max()
            .unwrap_or(1);
        self.node(Node::Alt(alternatives), factor)
    }
    fn parse(pattern: &JsString) -> IvyResult<(Vec<Node>, usize)> {
        let mut p = Self {
            input: points(pattern),
            at: 0,
            flags: Flags::default(),
            nodes: Vec::new(),
            factors: Vec::new(),
            frames: vec![Frame::new(Flags::default())],
            names: HashSet::new(),
        };
        while p.at < p.input.len() {
            let c = p.take()?;
            let mut repeat = false;
            match c {
                40 => {
                    if p.starts("?") {
                        p.at += 1;
                        p.group()?;
                    } else {
                        p.frames.push(Frame::new(p.flags));
                    }
                }
                41 => {
                    if p.frames.len() == 1 {
                        return Err(syntax());
                    }
                    let id = p.finish_frame();
                    let frame = p.frames.pop().unwrap();
                    p.flags = frame.flags;
                    p.frames.last_mut().unwrap().sequence.push(id);
                }
                124 => {
                    let id = p.finish_sequence();
                    p.frames.last_mut().unwrap().alternatives.push(id);
                }
                94 => p.push(
                    Node::Assert(if p.flags.multiline {
                        Assertion::LineStart
                    } else {
                        Assertion::Start
                    }),
                    1,
                ),
                36 => p.push(
                    Node::Assert(if p.flags.multiline {
                        Assertion::LineEnd
                    } else {
                        Assertion::End
                    }),
                    1,
                ),
                46 => p.push(
                    Node::Class(if p.flags.dotall {
                        Arc::from([0, 0x10ffff])
                    } else {
                        Arc::from([0, 9, 11, 0x10ffff])
                    }),
                    1,
                ),
                91 => {
                    let class = p.class()?;
                    p.push(Node::Class(class), 1);
                }
                42 | 43 | 63 => {
                    let (min, max) = match c {
                        42 => (0, None),
                        43 => (1, None),
                        _ => (0, Some(1)),
                    };
                    p.repeat(min, max, false)?;
                    repeat = true;
                }
                123 => {
                    // RE2JS remembers a brace as the previous repeat token even
                    // when its contents did not form a counted repetition.
                    if let Some((min, max)) = p.counted()? {
                        p.repeat(min, max, true)?;
                    } else {
                        p.literal(123);
                    }
                    repeat = true;
                }
                92 => match p.input.get(p.at).copied() {
                    Some(65 | 122 | 98 | 66) => {
                        let c = p.take()?;
                        p.push(
                            Node::Assert(match c {
                                65 => Assertion::Start,
                                122 => Assertion::End,
                                98 => Assertion::Word,
                                _ => Assertion::NotWord,
                            }),
                            1,
                        );
                    }
                    Some(81) => {
                        p.at += 1;
                        while p.at < p.input.len() && !p.starts("\\E") {
                            let c = p.take()?;
                            p.literal(c);
                        }
                        if p.starts("\\E") {
                            p.at += 2;
                        }
                    }
                    _ => {
                        p.at -= 1;
                        if let Some(class) = p.escape_class()? {
                            p.push(Node::Class(normalize(class)), 1);
                        } else {
                            let c = p.escape()?;
                            p.literal(c);
                        }
                    }
                },
                _ => p.literal(c),
            }
            p.frames.last_mut().unwrap().last_repeat = repeat;
        }
        if p.frames.len() != 1 {
            return Err(syntax());
        }
        let root = p.finish_frame();
        Ok((p.nodes, root))
    }
    fn group(&mut self) -> IvyResult<()> {
        if self.starts("P<") || self.starts("<") {
            self.at += if self.starts("P<") { 2 } else { 1 };
            let begin = self.at;
            while self.at < self.input.len() && self.input[self.at] != 62 {
                self.at += 1;
            }
            if self.at == self.input.len()
                || begin == self.at
                || !self.input[begin..self.at]
                    .iter()
                    .all(|c| ascii_alnum(*c) || *c == 95)
            {
                return Err(syntax());
            }
            let name = self.input[begin..self.at]
                .iter()
                .map(|c| *c as u8 as char)
                .collect();
            self.at += 1;
            if !self.names.insert(name) {
                return Err(syntax());
            }
            self.frames.push(Frame::new(self.flags));
            return Ok(());
        }
        let mut flags = self.flags;
        let mut negative = false;
        let mut saw = false;
        loop {
            match self.take()? {
                105 => {
                    flags.fold = !negative;
                    saw = true;
                }
                109 => {
                    flags.multiline = !negative;
                    saw = true;
                }
                115 => {
                    flags.dotall = !negative;
                    saw = true;
                }
                85 => {
                    saw = true;
                } // Greediness cannot change an existence query.
                45 if !negative => {
                    negative = true;
                    saw = false;
                }
                end @ (58 | 41) if !negative || saw => {
                    if end == 58 {
                        self.frames.push(Frame::new(self.flags));
                    }
                    self.flags = flags;
                    return Ok(());
                }
                _ => return Err(syntax()),
            }
        }
    }
    fn counted(&mut self) -> IvyResult<Option<(usize, Option<usize>)>> {
        let saved = self.at;
        let number = |p: &mut Self| -> Option<usize> {
            let begin = p.at;
            while p.input.get(p.at).is_some_and(|c| (48..=57).contains(c)) {
                p.at += 1;
            }
            let digits = &p.input[begin..p.at];
            if digits.is_empty() || (digits.len() > 1 && digits[0] == 48) {
                return None;
            }
            Some(digits.iter().fold(0usize, |v, c| {
                v.saturating_mul(10).saturating_add((*c - 48) as usize)
            }))
        };
        let parsed = (|| {
            let min = number(self)?;
            let max = if self.starts(",") {
                self.at += 1;
                if self.starts("}") {
                    None
                } else {
                    Some(number(self)?)
                }
            } else {
                Some(min)
            };
            if !self.starts("}") {
                return None;
            }
            self.at += 1;
            Some((min, max))
        })();
        if let Some((min, max)) = parsed {
            if min > 1000 || max.is_some_and(|n| n > 1000 || n < min) {
                return Err(syntax());
            }
            Ok(Some((min, max)))
        } else {
            self.at = saved;
            Ok(None)
        }
    }
    fn repeat(&mut self, min: usize, max: Option<usize>, counted: bool) -> IvyResult<()> {
        if self.starts("?") {
            self.at += 1;
        }
        let frame = self.frames.last_mut().unwrap();
        if frame.last_repeat {
            return Err(syntax());
        }
        let child = frame.sequence.pop().ok_or_else(syntax)?;
        let factor = if counted {
            if max == Some(0) {
                0
            } else {
                self.factors[child].saturating_mul(max.unwrap_or(min).max(1))
            }
        } else {
            self.factors[child]
        };
        if counted && (min >= 2 || max.is_some_and(|n| n >= 2)) && factor > 1000 {
            return Err(syntax());
        }
        self.push(Node::Repeat { child, min, max }, factor);
        Ok(())
    }
    fn escape(&mut self) -> IvyResult<u32> {
        if self.take()? != 92 {
            return Err(syntax());
        }
        let c = self.take()?;
        match c {
            48..=55
                if c == 48
                    || self
                        .input
                        .get(self.at)
                        .is_some_and(|c| (48..=55).contains(c)) =>
            {
                let mut point = c - 48;
                for _ in 1..3 {
                    if self
                        .input
                        .get(self.at)
                        .is_some_and(|c| (48..=55).contains(c))
                    {
                        point = point * 8 + self.take()? - 48;
                    } else {
                        break;
                    }
                }
                Ok(point)
            }
            120 => {
                let mut point = 0u32;
                if self.starts("{") {
                    self.at += 1;
                    let begin = self.at;
                    while !self.starts("}") {
                        let digit = hex(self.take()?).ok_or_else(syntax)?;
                        point = point
                            .checked_mul(16)
                            .and_then(|v| v.checked_add(digit))
                            .filter(|v| *v <= 0x10ffff)
                            .ok_or_else(syntax)?;
                    }
                    if self.at == begin {
                        return Err(syntax());
                    }
                    self.at += 1;
                } else {
                    for _ in 0..2 {
                        point = point * 16 + hex(self.take()?).ok_or_else(syntax)?;
                    }
                }
                Ok(point)
            }
            97 => Ok(7),
            102 => Ok(12),
            110 => Ok(10),
            114 => Ok(13),
            116 => Ok(9),
            118 => Ok(11),
            c if c <= 127 && !ascii_alnum(c) => Ok(c),
            _ => Err(syntax()),
        }
    }
    fn escape_class(&mut self) -> IvyResult<Option<Vec<(u32, u32)>>> {
        if !self.starts("\\") {
            return Ok(None);
        }
        if self.starts("\\p") || self.starts("\\P") {
            let mut neg = self.input[self.at + 1] == 80;
            self.at += 2;
            let name = if self.starts("{") {
                self.at += 1;
                let begin = self.at;
                while self.input.get(self.at).is_some_and(|c| *c != 125) {
                    self.at += 1;
                }
                if self.at == self.input.len() {
                    return Err(syntax());
                }
                let name = self.input[begin..self.at].to_vec();
                self.at += 1;
                name
            } else {
                vec![self.take()?]
            };
            let name = if name.first() == Some(&94) {
                neg = !neg;
                &name[1..]
            } else {
                &name
            };
            let name = name
                .iter()
                .map(|c| char::from_u32(*c).ok_or_else(syntax))
                .collect::<IvyResult<String>>()?;
            let mut ranges = self.group_ranges(&name)?;
            if neg {
                ranges = negate(&normalize(ranges));
            }
            return Ok(Some(ranges));
        }
        if let Some(c) = self.input.get(self.at + 1).copied() {
            if [100, 68, 115, 83, 119, 87].contains(&c) {
                self.at += 2;
                return self
                    .group_ranges(&format!("\\{}", c as u8 as char))
                    .map(Some);
            }
        }
        Ok(None)
    }
    fn group_ranges(&self, name: &str) -> IvyResult<Vec<(u32, u32)>> {
        let group = tables().groups.get(name).ok_or_else(syntax)?;
        Ok(group[usize::from(self.flags.fold)]
            .chunks_exact(2)
            .map(|r| (r[0], r[1]))
            .collect())
    }
    fn class(&mut self) -> IvyResult<Arc<[u32]>> {
        let neg = self.starts("^");
        if neg {
            self.at += 1;
        }
        let mut first = true;
        let mut ranges = Vec::new();
        while first || !self.starts("]") {
            first = false;
            if self.starts("[:") {
                if let Some(end) = self.input[self.at..].windows(2).position(|w| w == [58, 93]) {
                    let name = self.input[self.at..self.at + end + 2]
                        .iter()
                        .map(|c| char::from_u32(*c).ok_or_else(syntax))
                        .collect::<IvyResult<String>>()?;
                    self.at += end + 2;
                    ranges.extend(self.group_ranges(&name)?);
                    continue;
                }
            }
            if let Some(group) = self.escape_class()? {
                ranges.extend(group);
                continue;
            }
            let lo = if self.starts("\\") {
                self.escape()?
            } else {
                self.take()?
            };
            let mut hi = lo;
            if self.starts("-") && !self.starts("-]") {
                self.at += 1;
                hi = if self.starts("\\") {
                    self.escape()?
                } else {
                    self.take()?
                };
                if hi < lo {
                    return Err(syntax());
                }
            }
            let mut literal = vec![(lo, hi)];
            if self.flags.fold {
                fold(&mut literal);
            }
            ranges.extend(literal);
        }
        self.at += 1;
        let class = normalize(ranges);
        Ok(if neg {
            normalize(negate(&class))
        } else {
            class
        })
    }
}
fn ascii_alnum(c: u32) -> bool {
    (48..=57).contains(&c) || (65..=90).contains(&c) || (97..=122).contains(&c)
}
fn hex(c: u32) -> Option<u32> {
    match c {
        48..=57 => Some(c - 48),
        65..=70 => Some(c - 55),
        97..=102 => Some(c - 87),
        _ => None,
    }
}
fn normalize(mut ranges: Vec<(u32, u32)>) -> Arc<[u32]> {
    ranges.sort_unstable();
    let mut out: Vec<u32> = Vec::new();
    for (lo, hi) in ranges {
        if out.last().is_some_and(|end| lo <= end + 1) {
            let end = out.last_mut().unwrap();
            *end = (*end).max(hi);
        } else {
            out.extend([lo, hi]);
        }
    }
    Arc::from(out)
}
fn negate(ranges: &[u32]) -> Vec<(u32, u32)> {
    let mut next = 0;
    let mut out = Vec::new();
    for pair in ranges.chunks_exact(2) {
        if next < pair[0] {
            out.push((next, pair[0] - 1));
        }
        next = pair[1] + 1;
    }
    if next <= 0x10ffff {
        out.push((next, 0x10ffff));
    }
    out
}
fn fold(ranges: &mut Vec<(u32, u32)>) {
    let data = &tables().folds;
    let mut extra = Vec::new();
    for [from, to] in data {
        if ranges.iter().any(|(lo, hi)| lo <= from && from <= hi) {
            let mut point = *to;
            while point != *from {
                extra.push((point, point));
                point = data
                    .binary_search_by_key(&point, |pair| pair[0])
                    .ok()
                    .map_or(point, |i| data[i][1]);
                if point == *to {
                    break;
                }
            }
        }
    }
    ranges.extend(extra);
}
fn contains(ranges: &[u32], point: u32) -> bool {
    let mut lo = 0;
    let mut hi = ranges.len() / 2;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if point < ranges[mid * 2] {
            hi = mid;
        } else if point > ranges[mid * 2 + 1] {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

enum Instruction {
    Match,
    Class(Arc<[u32]>, usize),
    Assert(Assertion, usize),
    Split(usize, usize),
}
pub struct Re2Pattern {
    program: Vec<Instruction>,
    start: usize,
}
impl Re2Pattern {
    pub fn compile(pattern: &JsString) -> IvyResult<Self> {
        if pattern.0.len() > 1024 {
            return Err(IvyError::new(
                "registry_invalid",
                "Schema pattern exceeds its limit.",
            ));
        }
        let (nodes, root) = Parser::parse(pattern)?;
        enum Work {
            Build(usize, usize),
            Concat(usize, usize, usize),
            ResumeConcat(usize, usize),
            Alt(usize),
            Repeat(usize, usize, usize, usize),
            ResumeRepeat(usize, usize, usize, usize),
            Loop(usize, usize, usize, usize),
        }
        let mut program = vec![Instruction::Match];
        let mut work = vec![Work::Build(root, 0)];
        let mut results = Vec::new();
        while let Some(step) = work.pop() {
            match step {
                Work::Build(id, next) => match &nodes[id] {
                    Node::Empty => results.push(next),
                    Node::Class(class) => {
                        results.push(program.len());
                        program.push(Instruction::Class(class.clone(), next));
                    }
                    Node::Assert(kind) => {
                        results.push(program.len());
                        program.push(Instruction::Assert(*kind, next));
                    }
                    Node::Concat(children) => work.push(Work::Concat(id, children.len(), next)),
                    Node::Alt(children) => {
                        work.push(Work::Alt(children.len()));
                        for child in children {
                            work.push(Work::Build(*child, next));
                        }
                    }
                    Node::Repeat { child, min, max } => match max {
                        Some(max) => work.push(Work::Repeat(*child, *max, *min, next)),
                        None => {
                            let slot = program.len();
                            program.push(Instruction::Split(0, next));
                            work.push(Work::Loop(slot, *child, *min, next));
                            work.push(Work::Build(*child, slot));
                        }
                    },
                },
                Work::Concat(id, count, next) => {
                    if count == 0 {
                        results.push(next);
                    } else {
                        let Node::Concat(children) = &nodes[id] else {
                            unreachable!()
                        };
                        work.push(Work::ResumeConcat(id, count - 1));
                        work.push(Work::Build(children[count - 1], next));
                    }
                }
                Work::ResumeConcat(id, count) => {
                    work.push(Work::Concat(id, count, results.pop().unwrap()))
                }
                Work::Alt(count) => {
                    let mut start = results.pop().unwrap();
                    for _ in 1..count {
                        let other = results.pop().unwrap();
                        let slot = program.len();
                        program.push(Instruction::Split(start, other));
                        start = slot;
                    }
                    results.push(start);
                }
                Work::Repeat(child, count, min, next) => {
                    if count == 0 {
                        results.push(next);
                    } else {
                        work.push(Work::ResumeRepeat(child, count, min, next));
                        work.push(Work::Build(child, next));
                    }
                }
                Work::ResumeRepeat(child, count, min, next) => {
                    let mut start = results.pop().unwrap();
                    if count > min {
                        let slot = program.len();
                        program.push(Instruction::Split(start, next));
                        start = slot;
                    }
                    work.push(Work::Repeat(child, count - 1, min, start));
                }
                Work::Loop(slot, child, min, next) => {
                    let body = results.pop().unwrap();
                    program[slot] = Instruction::Split(body, next);
                    if min == 0 {
                        results.push(slot);
                    } else {
                        work.push(Work::Repeat(child, min - 1, min - 1, body));
                    }
                }
            }
        }
        Ok(Self {
            program,
            start: results.pop().unwrap(),
        })
    }
    pub fn is_match(&self, input: &JsString) -> bool {
        let input = points(input);
        let mut seen = vec![0usize; self.program.len()];
        let mut pending = Vec::new();
        let mut next = Vec::new();
        for position in 0..=input.len() {
            pending.push(self.start);
            while let Some(id) = pending.pop() {
                if seen[id] == position + 1 {
                    continue;
                }
                seen[id] = position + 1;
                match &self.program[id] {
                    Instruction::Match => return true,
                    Instruction::Class(ranges, dest) => {
                        if input
                            .get(position)
                            .is_some_and(|point| contains(ranges, *point))
                        {
                            next.push(*dest);
                        }
                    }
                    Instruction::Split(a, b) => {
                        pending.extend([*a, *b]);
                    }
                    Instruction::Assert(kind, dest) => {
                        let before = position.checked_sub(1).map(|i| input[i]);
                        let after = input.get(position).copied();
                        let word = |c: Option<u32>| c.is_some_and(|c| ascii_alnum(c) || c == 95);
                        let pass = match kind {
                            Assertion::Start => position == 0,
                            Assertion::End => position == input.len(),
                            Assertion::LineStart => position == 0 || before == Some(10),
                            Assertion::LineEnd => after.is_none() || after == Some(10),
                            Assertion::Word => word(before) != word(after),
                            Assertion::NotWord => word(before) == word(after),
                        };
                        if pass {
                            pending.push(*dest);
                        }
                    }
                }
            }
            std::mem::swap(&mut pending, &mut next);
        }
        false
    }
}
