//! A reader for the pickles Python 2 wrote.
//!
//! Protocols 0–2: the opcodes `cPickle` emits for the plain data of a
//! `MailList.__dict__` and the old-style class instances
//! (`Mailman.Bouncer._BounceInfo`) it keeps in `bounce_info`, plus the
//! protocol 3–4 opcodes a Python 3 `pickle.dumps` adds.
//!
//! `serde-pickle` 1.2.0 pops the class of an `OBJ` twice (the class sits
//! inside the mark) and so misreads every 2.1 pickle with bounce
//! information; this reader follows `pickle.py` instead. Memoised values
//! live in one arena, so a container filled after its `PUT` reads back
//! full wherever it is referenced, and a reference cycle reads as `None`.
use crate::{Error, Result};
use std::collections::HashMap;

/// A pickled value with Python's own types.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Text(String),
    List(Vec<Item>),
    Tuple(Vec<Item>),
    Dict(Vec<(Item, Item)>),
    /// A class or function by `module.name`.
    Global(String),
    /// A class instance: its `__dict__` (or `__setstate__` argument) once
    /// `BUILD` ran, else `None`.
    Instance(Box<Item>),
}

/// A value while the stream is read: `Item` with memo references.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Text(String),
    List(Vec<Node>),
    Tuple(Vec<Node>),
    Dict(Vec<(Node, Node)>),
    Global(String),
    Instance(Box<Node>),
    /// A memoised value, by arena index.
    Ref(usize),
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    stack: Vec<Node>,
    marks: Vec<usize>,
    arena: Vec<Node>,
    memo: HashMap<u64, usize>,
}

/// Read one pickle.
/// # Errors
/// Returns `Pickle` on a truncated stream, an opcode this reader does not
/// know, or a stack that does not fit the opcode.
pub fn read(bytes: &[u8]) -> Result<Item> {
    let mut reader = Reader {
        bytes,
        pos: 0,
        stack: Vec::new(),
        marks: Vec::new(),
        arena: Vec::new(),
        memo: HashMap::new(),
    };
    let node = reader.run()?;
    Ok(reader.resolve(node, &mut Vec::new()))
}

impl Reader<'_> {
    fn error<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(Error::Pickle(format!(
            "at byte {}: {}",
            self.pos,
            message.into()
        )))
    }

    fn take(&mut self, count: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len());
        let Some(end) = end else {
            return self.error("truncated pickle");
        };
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn line(&mut self) -> Result<String> {
        let start = self.pos;
        let Some(length) = self.bytes[start..].iter().position(|b| *b == b'\n') else {
            return self.error("unterminated line");
        };
        self.pos = start + length + 1;
        Ok(String::from_utf8_lossy(&self.bytes[start..start + length]).into_owned())
    }

    fn u8_len(&mut self) -> Result<usize> {
        Ok(usize::from(self.byte()?))
    }

    fn u32_len(&mut self) -> Result<usize> {
        let raw = self.take(4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize)
    }

    fn u64_len(&mut self) -> Result<usize> {
        let raw = self.take(8)?;
        let mut array = [0; 8];
        array.copy_from_slice(raw);
        usize::try_from(u64::from_le_bytes(array)).or_else(|_| self.error("length too large"))
    }

    fn pop(&mut self) -> Result<Node> {
        if self.stack.len() <= self.marks.last().copied().unwrap_or(0) {
            return self.error("stack underflow");
        }
        Ok(self.stack.pop().unwrap_or(Node::None))
    }

    fn pop_mark(&mut self) -> Result<Vec<Node>> {
        let Some(mark) = self.marks.pop() else {
            return self.error("no mark to pop");
        };
        Ok(self.stack.split_off(mark))
    }

    /// The value at the top of the stack, in the arena when memoised.
    fn top_mut(&mut self) -> Result<&mut Node> {
        let Some(last) = self.stack.len().checked_sub(1) else {
            return self.error("stack underflow");
        };
        if let Node::Ref(index) = self.stack[last] {
            return Ok(&mut self.arena[index]);
        }
        Ok(&mut self.stack[last])
    }

    fn put(&mut self, id: u64) -> Result<()> {
        let Some(last) = self.stack.len().checked_sub(1) else {
            return self.error("PUT on an empty stack");
        };
        let index = if let Node::Ref(index) = self.stack[last] {
            index
        } else {
            let index = self.arena.len();
            let item = std::mem::replace(&mut self.stack[last], Node::Ref(index));
            self.arena.push(item);
            index
        };
        self.memo.insert(id, index);
        Ok(())
    }

    fn get(&mut self, id: u64) -> Result<()> {
        match self.memo.get(&id) {
            Some(index) => {
                self.stack.push(Node::Ref(*index));
                Ok(())
            }
            None => self.error(format!("GET of memo {id} before its PUT")),
        }
    }

    /// A node's value, memo references resolved; a cycle is `None`.
    fn resolve(&self, node: Node, resolving: &mut Vec<usize>) -> Item {
        match node {
            Node::None => Item::None,
            Node::Bool(flag) => Item::Bool(flag),
            Node::Int(number) => Item::Int(number),
            Node::Float(number) => Item::Float(number),
            Node::Bytes(bytes) => Item::Bytes(bytes),
            Node::Text(text) => Item::Text(text),
            Node::Global(name) => Item::Global(name),
            Node::List(items) => Item::List(self.resolve_all(items, resolving)),
            Node::Tuple(items) => Item::Tuple(self.resolve_all(items, resolving)),
            Node::Dict(entries) => Item::Dict(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        (self.resolve(key, resolving), self.resolve(value, resolving))
                    })
                    .collect(),
            ),
            Node::Instance(state) => Item::Instance(Box::new(self.resolve(*state, resolving))),
            Node::Ref(index) => {
                if resolving.contains(&index) {
                    return Item::None;
                }
                resolving.push(index);
                let item = self.resolve(self.arena[index].clone(), resolving);
                resolving.pop();
                item
            }
        }
    }

    fn resolve_all(&self, nodes: Vec<Node>, resolving: &mut Vec<usize>) -> Vec<Item> {
        nodes
            .into_iter()
            .map(|node| self.resolve(node, resolving))
            .collect()
    }

    fn run(&mut self) -> Result<Node> {
        loop {
            let opcode = self.byte()?;
            match opcode {
                b'.' => return self.pop(),
                b'\x80' => {
                    self.byte()?;
                }
                b'\x95' => {
                    self.take(8)?;
                }
                b'(' => self.marks.push(self.stack.len()),
                b'0' => {
                    self.pop()?;
                }
                b'1' => {
                    self.pop_mark()?;
                }
                b'2' => {
                    let Some(top) = self.stack.last().cloned() else {
                        return self.error("DUP on an empty stack");
                    };
                    self.stack.push(top);
                }
                b'N' => self.stack.push(Node::None),
                b'\x88' => self.stack.push(Node::Bool(true)),
                b'\x89' => self.stack.push(Node::Bool(false)),
                b'I' | b'L' | b'F' | b'S' | b'V' => self.text_scalar(opcode)?,
                b'K' | b'M' | b'J' | b'\x8a' | b'\x8b' | b'G' => self.binary_number(opcode)?,
                b'T' | b'U' | b'B' | b'C' | b'\x8e' | b'X' | b'\x8c' | b'\x8d' => {
                    self.binary_string(opcode)?;
                }
                b')' => self.stack.push(Node::Tuple(Vec::new())),
                b']' | b'\x8f' => self.stack.push(Node::List(Vec::new())),
                b'}' => self.stack.push(Node::Dict(Vec::new())),
                b't' | b'l' | b'd' | b'\x91' => self.collect_mark(opcode)?,
                b'\x85' | b'\x86' | b'\x87' => {
                    let count = usize::from(opcode - b'\x84');
                    let base = self.marks.last().copied().unwrap_or(0);
                    let Some(start) = self.stack.len().checked_sub(count).filter(|s| *s >= base)
                    else {
                        return self.error("TUPLEn on a short stack");
                    };
                    let items = self.stack.split_off(start);
                    self.stack.push(Node::Tuple(items));
                }
                b'a' | b'e' | b'\x90' => self.append(opcode)?,
                b's' | b'u' => self.set_items(opcode)?,
                b'p' | b'g' | b'q' | b'r' | b'h' | b'j' | b'\x94' => self.memo_op(opcode)?,
                b'c' | b'\x93' | b'i' | b'o' | b'\x81' | b'\x92' | b'R' | b'b' => {
                    self.object_op(opcode)?;
                }
                other => return self.error(format!("unsupported opcode {other:#04x}")),
            }
        }
    }

    fn text_scalar(&mut self, opcode: u8) -> Result<()> {
        let line = self.line()?;
        let node = match opcode {
            b'I' => match line.as_str() {
                "01" => Node::Bool(true),
                "00" => Node::Bool(false),
                _ => Node::Int(self.parse(&line)?),
            },
            b'L' => Node::Int(self.parse(line.trim_end_matches('L'))?),
            b'F' => Node::Float(self.parse(&line)?),
            b'S' => Node::Bytes(self.unquote(&line)?),
            _ => Node::Text(unescape_unicode(&line)),
        };
        self.stack.push(node);
        Ok(())
    }

    fn parse<T: std::str::FromStr>(&self, text: &str) -> Result<T> {
        text.parse()
            .or_else(|_| self.error(format!("bad number {text:?}")))
    }

    /// A Python 2 `repr` of a `str`.
    fn unquote(&self, line: &str) -> Result<Vec<u8>> {
        let inner = line
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .or_else(|| {
                line.strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))
            });
        let Some(inner) = inner else {
            return self.error(format!("bad string literal {line:?}"));
        };
        let mut out = Vec::new();
        let mut chars = inner.bytes();
        while let Some(byte) = chars.next() {
            if byte != b'\\' {
                out.push(byte);
                continue;
            }
            match chars.next() {
                Some(b'n') => out.push(b'\n'),
                Some(b't') => out.push(b'\t'),
                Some(b'r') => out.push(b'\r'),
                Some(b'x') => {
                    let hex: String = chars.by_ref().take(2).map(char::from).collect();
                    out.push(u8::from_str_radix(&hex, 16).unwrap_or(b'?'));
                }
                Some(other) => out.push(other),
                None => break,
            }
        }
        Ok(out)
    }

    fn binary_number(&mut self, opcode: u8) -> Result<()> {
        let node = match opcode {
            b'K' => Node::Int(i64::from(self.byte()?)),
            b'M' => {
                let raw = self.take(2)?;
                Node::Int(i64::from(u16::from_le_bytes([raw[0], raw[1]])))
            }
            b'J' => {
                let raw = self.take(4)?;
                Node::Int(i64::from(i32::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3],
                ])))
            }
            b'G' => {
                let raw = self.take(8)?;
                let mut array = [0; 8];
                array.copy_from_slice(raw);
                Node::Float(f64::from_be_bytes(array))
            }
            _ => {
                let length = if opcode == b'\x8a' {
                    self.u8_len()?
                } else {
                    self.u32_len()?
                };
                let raw = self.take(length)?;
                let mut array = [0; 8];
                if raw.len() > 8 {
                    return self.error("integer too large");
                }
                array[..raw.len()].copy_from_slice(raw);
                if raw.last().is_some_and(|b| b & 0x80 != 0) {
                    for byte in &mut array[raw.len()..] {
                        *byte = 0xff;
                    }
                }
                Node::Int(i64::from_le_bytes(array))
            }
        };
        self.stack.push(node);
        Ok(())
    }

    fn binary_string(&mut self, opcode: u8) -> Result<()> {
        let length = match opcode {
            b'U' | b'C' | b'\x8c' => self.u8_len()?,
            b'T' | b'B' | b'X' => self.u32_len()?,
            _ => self.u64_len()?,
        };
        let raw = self.take(length)?.to_vec();
        let node = match opcode {
            b'X' | b'\x8c' | b'\x8d' => Node::Text(String::from_utf8_lossy(&raw).into_owned()),
            _ => Node::Bytes(raw),
        };
        self.stack.push(node);
        Ok(())
    }

    fn collect_mark(&mut self, opcode: u8) -> Result<()> {
        let items = self.pop_mark()?;
        let node = match opcode {
            b't' => Node::Tuple(items),
            b'd' => {
                if items.len() % 2 != 0 {
                    return self.error("DICT with an odd number of items");
                }
                let mut entries = Vec::with_capacity(items.len() / 2);
                let mut items = items.into_iter();
                while let (Some(key), Some(value)) = (items.next(), items.next()) {
                    entries.push((key, value));
                }
                Node::Dict(entries)
            }
            _ => Node::List(items),
        };
        self.stack.push(node);
        Ok(())
    }

    fn append(&mut self, opcode: u8) -> Result<()> {
        let items = if opcode == b'a' {
            vec![self.pop()?]
        } else {
            self.pop_mark()?
        };
        if let Node::List(list) = self.top_mut()? {
            list.extend(items);
            return Ok(());
        }
        self.error("APPEND to something that is not a list")
    }

    fn set_items(&mut self, opcode: u8) -> Result<()> {
        let items = if opcode == b's' {
            let value = self.pop()?;
            let key = self.pop()?;
            vec![key, value]
        } else {
            self.pop_mark()?
        };
        if items.len() % 2 != 0 {
            return self.error("SETITEMS with an odd number of items");
        }
        let mut pairs = Vec::with_capacity(items.len() / 2);
        let mut items = items.into_iter();
        while let (Some(key), Some(value)) = (items.next(), items.next()) {
            pairs.push((key, value));
        }
        if let Node::Dict(entries) = self.top_mut()? {
            entries.extend(pairs);
            return Ok(());
        }
        self.error("SETITEM on something that is not a dict")
    }

    fn memo_op(&mut self, opcode: u8) -> Result<()> {
        match opcode {
            b'p' => {
                let id = self.line()?;
                let id = self.parse(&id)?;
                self.put(id)
            }
            b'g' => {
                let id = self.line()?;
                let id = self.parse(&id)?;
                self.get(id)
            }
            b'q' => {
                let id = self.u8_len()?;
                self.put(id as u64)
            }
            b'r' => {
                let id = self.u32_len()?;
                self.put(id as u64)
            }
            b'h' => {
                let id = self.u8_len()?;
                self.get(id as u64)
            }
            b'j' => {
                let id = self.u32_len()?;
                self.get(id as u64)
            }
            _ => {
                let id = self.memo.len() as u64;
                self.put(id)
            }
        }
    }

    fn object_op(&mut self, opcode: u8) -> Result<()> {
        match opcode {
            b'c' => {
                let module = self.line()?;
                let name = self.line()?;
                self.stack.push(Node::Global(format!("{module}.{name}")));
            }
            b'\x93' => {
                let name = self.pop()?;
                let module = self.pop()?;
                let (Node::Text(module), Node::Text(name)) = (module, name) else {
                    return self.error("STACK_GLOBAL without two strings");
                };
                self.stack.push(Node::Global(format!("{module}.{name}")));
            }
            b'i' => {
                self.line()?;
                self.line()?;
                self.pop_mark()?;
                self.stack.push(Node::Instance(Box::new(Node::None)));
            }
            b'o' => {
                // The class is the first item after the mark.
                let args = self.pop_mark()?;
                if args.is_empty() {
                    return self.error("OBJ without a class");
                }
                self.stack.push(Node::Instance(Box::new(Node::None)));
            }
            b'\x81' => {
                self.pop()?;
                self.pop()?;
                self.stack.push(Node::Instance(Box::new(Node::None)));
            }
            b'\x92' => {
                self.pop()?;
                self.pop()?;
                self.pop()?;
                self.stack.push(Node::Instance(Box::new(Node::None)));
            }
            b'R' => {
                let args = self.pop()?;
                let callable = self.pop()?;
                let node = self.reduce(callable, args);
                self.stack.push(node);
            }
            _ => {
                let state = self.pop()?;
                match self.top_mut()? {
                    Node::Instance(slot) => *slot = Box::new(state),
                    other => *other = state,
                }
            }
        }
        Ok(())
    }

    /// `REDUCE`: the builtins Python 2 pickles through a call, else an
    /// instance.
    fn reduce(&self, callable: Node, args: Node) -> Node {
        let deref = |node: Node| match node {
            Node::Ref(index) => self.arena[index].clone(),
            other => other,
        };
        let Node::Global(name) = deref(callable) else {
            return Node::Instance(Box::new(Node::None));
        };
        let mut args = match deref(args) {
            Node::Tuple(items) => items,
            _ => Vec::new(),
        };
        match name.as_str() {
            "__builtin__.set"
            | "builtins.set"
            | "__builtin__.frozenset"
            | "builtins.frozenset"
            | "__builtin__.list"
            | "builtins.list"
            | "__builtin__.tuple"
            | "builtins.tuple" => args.pop().map_or_else(|| Node::List(Vec::new()), deref),
            // Python 3 pickles `bytes` at protocol 2 as
            // `_codecs.encode(text, 'latin1')`.
            "_codecs.encode" => match args.into_iter().next().map(deref) {
                Some(Node::Text(text)) => Node::Bytes(text.chars().map(|c| c as u8).collect()),
                _ => Node::Bytes(Vec::new()),
            },
            _ => Node::Instance(Box::new(Node::None)),
        }
    }
}

/// `raw-unicode-escape`: only `\uXXXX` and `\UXXXXXXXX` are escapes.
fn unescape_unicode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let width = match chars.peek() {
            Some('u') => 4,
            Some('U') => 8,
            _ => {
                out.push('\\');
                continue;
            }
        };
        chars.next();
        let hex: String = chars.by_ref().take(width).collect();
        if let Some(decoded) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
            out.push(decoded);
        } else {
            out.push('\\');
            out.push_str(&hex);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Item, read};

    #[test]
    fn an_obj_instance_keeps_its_class_inside_the_mark() {
        // {'a': _BounceInfo(score=1.0), 'b': 2}, protocol 2 as Python 2 wrote it.
        let pickle = b"\x80\x02}q\x00(U\x01aq\x01(cMailman.Bouncer\n_BounceInfo\nq\x02o}q\x03U\x05scoreq\x04G\x3f\xf0\x00\x00\x00\x00\x00\x00sbU\x01bq\x05K\x02u.";
        let Item::Dict(entries) = read(pickle).unwrap() else {
            panic!("dict");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, Item::Bytes(b"a".to_vec()));
        assert_eq!(
            entries[0].1,
            Item::Instance(Box::new(Item::Dict(vec![(
                Item::Bytes(b"score".to_vec()),
                Item::Float(1.0)
            )])))
        );
        assert_eq!(entries[1], (Item::Bytes(b"b".to_vec()), Item::Int(2)));
    }

    #[test]
    fn a_container_filled_after_its_put_is_full_everywhere_it_is_referenced() {
        // l = [1, 2]; (l, l)
        let pickle = b"\x80\x02]q\x00(K\x01K\x02eh\x00h\x00\x86q\x01.";
        let list = Item::List(vec![Item::Int(1), Item::Int(2)]);
        assert_eq!(read(pickle).unwrap(), Item::Tuple(vec![list.clone(), list]));
    }

    #[test]
    fn python_3_bytes_at_protocol_2_come_back_as_bytes() {
        // pickle.dumps({b'k': b'v', b'k2': b'v'}, protocol=2): `_codecs.encode`,
        // memoised, called through REDUCE with memoised arguments.
        let pickle = b"\x80\x02}q\x00(c_codecs\nencode\nq\x01X\x01\x00\x00\x00kq\x02X\x06\x00\x00\x00latin1q\x03\x86q\x04Rq\x05h\x01X\x01\x00\x00\x00vq\x06h\x03\x86q\x07Rq\x08h\x01X\x02\x00\x00\x00k2q\th\x03\x86q\nRq\x0bh\x08u.";
        assert_eq!(
            read(pickle).unwrap(),
            Item::Dict(vec![
                (Item::Bytes(b"k".to_vec()), Item::Bytes(b"v".to_vec())),
                (Item::Bytes(b"k2".to_vec()), Item::Bytes(b"v".to_vec())),
            ])
        );
    }

    #[test]
    fn a_reference_cycle_reads_as_none_instead_of_looping() {
        // l = []; l.append(l)
        let pickle = b"\x80\x02]q\x00h\x00a.";
        assert_eq!(read(pickle).unwrap(), Item::List(vec![Item::None]));
    }

    #[test]
    fn protocol_0_text_opcodes_read() {
        // {'name': u'Phạm', 'n': 3L, 'f': 1.5, 't': (True, None), 'i': inst}
        let pickle = b"(dp0\nS'name'\np1\nVPh\\u1ea1m\np2\nsS'n'\np3\nL3L\nsS'f'\np4\nF1.5\nsS't'\np5\n(I01\nNtp6\nsS'i'\np7\n(iMailman.Bouncer\n_BounceInfo\np8\n(dp9\nS'x'\np10\nI1\nsbs.";
        let Item::Dict(entries) = read(pickle).unwrap() else {
            panic!("dict");
        };
        let value = |key: &[u8]| {
            entries
                .iter()
                .find(|entry| entry.0 == Item::Bytes(key.to_vec()))
                .map(|entry| entry.1.clone())
                .unwrap()
        };
        assert_eq!(value(b"name"), Item::Text("Phạm".into()));
        assert_eq!(value(b"n"), Item::Int(3));
        assert_eq!(value(b"f"), Item::Float(1.5));
        assert_eq!(value(b"t"), Item::Tuple(vec![Item::Bool(true), Item::None]));
        assert_eq!(
            value(b"i"),
            Item::Instance(Box::new(Item::Dict(vec![(
                Item::Bytes(b"x".to_vec()),
                Item::Int(1)
            )])))
        );
    }

    #[test]
    fn a_truncated_or_unknown_stream_is_an_error() {
        assert!(read(b"\x80\x02}q\x00(U\x01a").is_err());
        assert!(read(b"\x80\x02\xff.").is_err());
        assert!(read(b"").is_err());
    }
}
