//! Appending objects to an existing PDF as an **incremental update** (ISO 32000-1 7.5.6).
//!
//! The source's bytes are kept unchanged as the output's prefix. After them come the new and
//! replaced objects, a new cross-reference section listing only those, and its trailer, whose
//! `/Prev` points at the source's last section. The section is a table when the source's last one
//! is, else a cross-reference stream (which is itself an object, listed in its own index). New
//! objects are numbered after the file's highest (its trailer's `/Size`); a replaced object keeps
//! its number and generation. An encrypted PDF is refused: its new objects would have to be
//! encrypted too.
//!
//! Objects are written as text the caller gives ([`IncrementalUpdate::write`]); the helpers
//! [`write_object`], [`write_dict`] and [`write_entries`] write an object read by `hayro-syntax`
//! back out as it reads, its references unchanged, so an existing object can be rewritten with
//! some entries changed. The output is deterministic: the same source and the same objects give
//! the same bytes.
//!
//! Model: claude-opus-5-5 (medium effort, sp-worker) -- designed and written, 2026-10-30.

use hayro_syntax::object::{Dict, MaybeRef, Object};
use std::fmt::{self, Write as _};

/// Why an incremental update cannot be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The source's last cross-reference section or its trailer cannot be read: what is missing.
    Unreadable(&'static str),
    /// The source is encrypted.
    Encrypted,
    /// An object to be written out contains a stream where only a direct object can be.
    StreamInDirectObject,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "the PDF's end cannot be read: {why}"),
            Self::Encrypted => f.write_str("the PDF is encrypted"),
            Self::StreamInDirectObject => {
                f.write_str("the PDF has a stream where a direct object is")
            }
        }
    }
}

impl std::error::Error for UpdateError {}

/// What an update needs of the source's last cross-reference section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Where it starts (the source's last `startxref`).
    pub prev: usize,
    /// It is a cross-reference stream (else a table).
    pub stream: bool,
    /// Its trailer's `/Size`: one more than the highest object number.
    pub size: i32,
    /// Its trailer's `/Root`, `/Info` and `/ID`, as written.
    pub root: String,
    /// See [`Tail::root`].
    pub info: Option<String>,
    /// See [`Tail::root`].
    pub id: Option<String>,
}

/// How far from the end the last `startxref` is looked for.
const TAIL_WINDOW: usize = 4096;

/// The source's last cross-reference section and its trailer. Refused for an encrypted PDF, or
/// one whose end cannot be read.
pub fn read_tail(src: &[u8]) -> Result<Tail, UpdateError> {
    let from = src.len().saturating_sub(TAIL_WINDOW);
    let prev = last_startxref(&src[from..]).ok_or(UpdateError::Unreadable("no startxref"))?;
    if prev >= src.len() {
        return Err(UpdateError::Unreadable("startxref points past the end"));
    }
    let at = &src[prev..];
    let skip = at.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let stream = !at[skip..].starts_with(b"xref");
    // The trailer: a table's after the word `trailer`; a stream's is its dictionary.
    let region: &[u8] = if stream {
        let end = find(at, b"stream").unwrap_or(at.len().min(TAIL_WINDOW));
        &at[..end]
    } else {
        let t = find(at, b"trailer").ok_or(UpdateError::Unreadable("no trailer"))?;
        let rest = &at[t..];
        let end = find(rest, b"startxref").unwrap_or(rest.len());
        &rest[..end]
    };
    if find(region, b"/Encrypt").is_some() {
        return Err(UpdateError::Encrypted);
    }
    let text = |key: &[u8], value: fn(&[u8]) -> Option<usize>| {
        first_value(region, key, value).map(|v| String::from_utf8_lossy(v).into_owned())
    };
    Ok(Tail {
        prev,
        stream,
        size: text(b"/Size", digits)
            .and_then(|s| s.parse().ok())
            .ok_or(UpdateError::Unreadable("no /Size"))?,
        root: text(b"/Root", reference).ok_or(UpdateError::Unreadable("no /Root"))?,
        info: text(b"/Info", reference),
        id: first_value(region, b"/ID", id_array).map(|v| String::from_utf8_lossy(v).into_owned()),
    })
}

/// The number after the last `startxref` (and its whitespace) in `hay`.
fn last_startxref(hay: &[u8]) -> Option<usize> {
    let key = b"startxref";
    let mut found = None;
    let mut i = 0;
    while let Some(p) = find(&hay[i..], key) {
        let after = &hay[i + p + key.len()..];
        let ws = after.iter().take_while(|b| b.is_ascii_whitespace()).count();
        if ws > 0
            && let Some(n) = digits(&after[ws..])
        {
            found = Some(&after[ws..ws + n]);
        }
        i += p + 1;
    }
    std::str::from_utf8(found?).ok()?.parse().ok()
}

/// The first `key` in `region` that whitespace and then a value as `value` reads follow: the
/// value's bytes. (`/ID` takes no whitespace before its array.)
fn first_value<'a>(
    region: &'a [u8],
    key: &[u8],
    value: fn(&[u8]) -> Option<usize>,
) -> Option<&'a [u8]> {
    let mut i = 0;
    while let Some(p) = find(&region[i..], key) {
        let after = &region[i + p + key.len()..];
        let ws = after.iter().take_while(|b| b.is_ascii_whitespace()).count();
        if (ws > 0 || key == b"/ID")
            && let Some(n) = value(&after[ws..])
        {
            return Some(&after[ws..ws + n]);
        }
        i += p + 1;
    }
    None
}

/// The length of the run of ASCII digits at the start of `s`, when there is one.
fn digits(s: &[u8]) -> Option<usize> {
    let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
    (n > 0).then_some(n)
}

/// The length of a reference `n g R` at the start of `s`.
fn reference(s: &[u8]) -> Option<usize> {
    let mut i = digits(s)?;
    let ws = |s: &[u8]| {
        let n = s.iter().take_while(|b| b.is_ascii_whitespace()).count();
        (n > 0).then_some(n)
    };
    i += ws(&s[i..])?;
    i += digits(&s[i..])?;
    i += ws(&s[i..])?;
    (s.get(i) == Some(&b'R')).then_some(i + 1)
}

/// The length of an `/ID` array `[<hex> <hex>]` at the start of `s`.
fn id_array(s: &[u8]) -> Option<usize> {
    let ws = |s: &[u8]| s.iter().take_while(|b| b.is_ascii_whitespace()).count();
    let mut i = 0;
    if s.first() != Some(&b'[') {
        return None;
    }
    i += 1;
    for _ in 0..2 {
        i += ws(&s[i..]);
        if s.get(i) != Some(&b'<') {
            return None;
        }
        i += 1;
        i += s[i..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit() || b.is_ascii_whitespace())
            .count();
        if s.get(i) != Some(&b'>') {
            return None;
        }
        i += 1;
    }
    i += ws(&s[i..]);
    (s.get(i) == Some(&b']')).then_some(i + 1)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// An incremental update being built on a source PDF ([module docs](self)).
#[derive(Debug, Clone)]
pub struct IncrementalUpdate<'a> {
    src: &'a [u8],
    tail: Tail,
    next: i32,
    /// (number, generation, body without `n g obj` and `endobj`)
    objects: Vec<(i32, u16, Vec<u8>)>,
}

impl<'a> IncrementalUpdate<'a> {
    /// An update on `src`, with nothing in it yet ([`read_tail`]'s refusals).
    pub fn new(src: &'a [u8]) -> Result<Self, UpdateError> {
        let tail = read_tail(src)?;
        let next = tail.size.max(1);
        Ok(Self {
            src,
            tail,
            next,
            objects: Vec::new(),
        })
    }

    /// The source's last cross-reference section, as read.
    pub fn tail(&self) -> &Tail {
        &self.tail
    }

    /// A new object number, after the file's highest and those given before.
    pub fn alloc(&mut self) -> i32 {
        let n = self.next;
        self.next += 1;
        n
    }

    /// Writes object `number` (generation `generation`) with `body`, the object without its
    /// `n g obj` and `endobj`: a new one from [`IncrementalUpdate::alloc`], or a new version of
    /// one of the source's. A second write of the same object replaces the first.
    pub fn write(&mut self, number: i32, generation: u16, body: Vec<u8>) {
        match self
            .objects
            .iter_mut()
            .find(|o| o.0 == number && o.1 == generation)
        {
            Some(o) => o.2 = body,
            None => self.objects.push((number, generation, body)),
        }
    }

    /// The source, then the objects in number order, the new cross-reference section and its
    /// trailer. With no objects written, the source unchanged.
    pub fn finish(self) -> Vec<u8> {
        if self.objects.is_empty() {
            return self.src.to_vec();
        }
        let Self {
            src,
            tail,
            next: size,
            mut objects,
        } = self;
        let mut out = src.to_vec();
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        objects.sort_by_key(|o| o.0);
        let mut offsets: Vec<(i32, u16, usize)> = Vec::new();
        for (n, g, body) in &objects {
            offsets.push((*n, *g, out.len()));
            out.extend_from_slice(format!("{n} {g} obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let mut trailer = format!("/Root {} /Prev {}", tail.root, tail.prev);
        if let Some(i) = &tail.info {
            let _ = write!(trailer, " /Info {i}");
        }
        if let Some(id) = &tail.id {
            let _ = write!(trailer, " /ID {id}");
        }
        let start = out.len();
        if tail.stream {
            // The stream itself is an object too, and listed.
            let me = size;
            offsets.push((me, 0, start));
            let mut data = Vec::new();
            let mut index = String::new();
            for run in runs(&offsets) {
                let _ = write!(index, "{} {} ", run[0].0, run.len());
                for (_, g, off) in run {
                    data.push(1_u8);
                    data.extend_from_slice(&(off as u32).to_be_bytes());
                    data.extend_from_slice(&g.to_be_bytes());
                }
            }
            out.extend_from_slice(
                format!(
                    "{me} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Index [{}] {trailer} /Length {} >>\nstream\n",
                    me + 1,
                    index.trim_end(),
                    data.len()
                )
                .as_bytes(),
            );
            out.extend_from_slice(&data);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        } else {
            let mut x = String::from("xref\n");
            for run in runs(&offsets) {
                let _ = writeln!(x, "{} {}", run[0].0, run.len());
                for (_, g, off) in run {
                    let _ = write!(x, "{off:010} {g:05} n\r\n");
                }
            }
            let _ = write!(x, "trailer\n<< /Size {size} {trailer} >>\n");
            out.extend_from_slice(x.as_bytes());
        }
        out.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
        out
    }
}

/// The entries in runs of consecutive object numbers.
fn runs(offsets: &[(i32, u16, usize)]) -> Vec<Vec<(i32, u16, usize)>> {
    let mut sorted = offsets.to_vec();
    sorted.sort_by_key(|o| o.0);
    let mut out: Vec<Vec<(i32, u16, usize)>> = Vec::new();
    for o in sorted {
        match out.last_mut() {
            Some(run) if run.last().is_some_and(|l| l.0 + 1 == o.0) => run.push(o),
            _ => out.push(vec![o]),
        }
    }
    out
}

// ---------------------------------------------------------------------- writing objects

/// A stream object's body (for [`IncrementalUpdate::write`]): `data` unfiltered, with its length.
pub fn stream_object(data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// A number as a PDF writes it: an integer when whole, else at most four decimals.
pub fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".into() } else { s.to_string() }
    }
}

/// A name as PDF writes it, with its `/`: `#xx` for anything outside the regular characters.
pub fn name(bytes: &[u8]) -> String {
    let mut s = String::from("/");
    for &b in bytes {
        if (0x21..=0x7e).contains(&b) && !b"()<>[]{}/%#".contains(&b) {
            s.push(b as char);
        } else {
            let _ = write!(s, "#{b:02X}");
        }
    }
    s
}

/// `v` as it reads: a reference as `n g R`, else the object.
pub fn write_maybe(out: &mut String, v: &MaybeRef<Object<'_>>) -> Result<(), UpdateError> {
    match v {
        MaybeRef::Ref(r) => {
            let _ = write!(out, "{} {} R", r.obj_number, r.gen_number);
            Ok(())
        }
        MaybeRef::NotRef(o) => write_object(out, o),
    }
}

/// `o` as it reads, its references unchanged (strings as hex). A stream is refused: it can only
/// be an object of its own.
pub fn write_object(out: &mut String, o: &Object<'_>) -> Result<(), UpdateError> {
    match o {
        Object::Null(_) => *out += "null",
        Object::Boolean(b) => *out += if *b { "true" } else { "false" },
        Object::Number(n) => *out += &number(n.as_f64()),
        Object::String(s) => {
            out.push('<');
            for b in s.as_bytes() {
                let _ = write!(out, "{b:02X}");
            }
            out.push('>');
        }
        Object::Name(n) => *out += &name(n.as_ref()),
        Object::Array(a) => {
            out.push('[');
            for (i, item) in a.raw_iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write_maybe(out, &item)?;
            }
            out.push(']');
        }
        Object::Dict(d) => write_dict(out, d, &[])?,
        Object::Stream(_) => return Err(UpdateError::StreamInDirectObject),
    }
    Ok(())
}

/// `d` as `<< ... >>`, the entries named in `skip` left out.
pub fn write_dict(out: &mut String, d: &Dict<'_>, skip: &[&[u8]]) -> Result<(), UpdateError> {
    *out += "<<";
    write_entries(out, d, skip)?;
    *out += ">>";
    Ok(())
}

/// `d`'s entries, each after a space, those named in `skip` left out (for a dictionary whose
/// other entries the caller adds).
pub fn write_entries(out: &mut String, d: &Dict<'_>, skip: &[&[u8]]) -> Result<(), UpdateError> {
    for (k, v) in d.entries() {
        if skip.contains(&k.as_ref()) {
            continue;
        }
        out.push(' ');
        *out += &name(k.as_ref());
        out.push(' ');
        write_maybe(out, &v)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::Pdf;
    use hayro_syntax::object::ObjectIdentifier;

    /// A one-page PDF from pdf-writer (a cross-reference table).
    fn table_pdf() -> Vec<u8> {
        use pdf_writer::{Content, Pdf, Rect, Ref};
        let mut pdf = Pdf::new();
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        pdf.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(Rect::new(0.0, 0.0, 200.0, 100.0))
            .contents(Ref::new(4));
        let mut c = Content::new();
        c.rect(10.0, 10.0, 20.0, 20.0).fill_nonzero();
        pdf.stream(Ref::new(4), &c.finish());
        pdf.finish()
    }

    /// A one-page PDF written by hand with a cross-reference stream (PDF 1.5) and an `/ID`.
    fn stream_pdf() -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut offs = Vec::new();
        let objs: [&[u8]; 3] = [
            b"<< /Type /Catalog /Pages 2 0 R >>",
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
        ];
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        let mut data = vec![0_u8, 0, 0, 0, 0, 0xff, 0xff];
        for o in offs.iter().chain([&xref_at]) {
            data.push(1);
            data.extend_from_slice(&(*o as u32).to_be_bytes());
            data.extend_from_slice(&[0, 0]);
        }
        out.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Root 1 0 R /ID [<00AB> <00ab>] /Length {} >>\nstream\n",
                data.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&data);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );
        out
    }

    /// Every entry of the last cross-reference table in `pdf` (`n g` -> offset) points at
    /// `n g obj`: the update's offsets are exact, so a reader needs no repair to open it.
    fn assert_table_offsets(pdf: &[u8]) {
        let tail = read_tail(pdf).unwrap();
        assert!(!tail.stream);
        let section = &pdf[tail.prev..];
        let text = String::from_utf8_lossy(&section[..find(section, b"trailer").unwrap()]);
        let mut lines = text.lines().skip(1);
        let mut checked = 0;
        while let Some(head) = lines.next() {
            let mut h = head.split_whitespace();
            let first: i32 = h.next().unwrap().parse().unwrap();
            let count: i32 = h.next().unwrap().parse().unwrap();
            for n in first..first + count {
                let entry = lines.next().unwrap();
                let off: usize = entry[..10].parse().unwrap();
                let g: u16 = entry[11..16].parse().unwrap();
                let want = format!("{n} {g} obj");
                assert!(pdf[off..].starts_with(want.as_bytes()), "{want} at {off}");
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    /// The same for a last cross-reference stream (unfiltered, `/W [1 4 2]`, as written here).
    fn assert_stream_offsets(pdf: &[u8]) {
        let tail = read_tail(pdf).unwrap();
        assert!(tail.stream);
        let section = &pdf[tail.prev..];
        let head = String::from_utf8_lossy(&section[..find(section, b"stream").unwrap()]);
        let index = &head[head.find("/Index [").unwrap() + 8..];
        let index: Vec<i32> = index[..index.find(']').unwrap()]
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        let mut data = &section[find(section, b"stream\n").unwrap() + 7..];
        let mut checked = 0;
        for pair in index.chunks(2) {
            for n in pair[0]..pair[0] + pair[1] {
                assert_eq!(data[0], 1);
                let off = u32::from_be_bytes(data[1..5].try_into().unwrap()) as usize;
                let g = u16::from_be_bytes(data[5..7].try_into().unwrap());
                let want = format!("{n} {g} obj");
                assert!(pdf[off..].starts_with(want.as_bytes()), "{want} at {off}");
                data = &data[7..];
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    #[test]
    fn reads_a_table_tail() {
        let src = table_pdf();
        let t = read_tail(&src).unwrap();
        assert!(!t.stream);
        assert_eq!(t.size, 5);
        assert_eq!(t.root, "1 0 R");
        assert_eq!(t.info, None);
        assert_eq!(t.id, None);
        assert!(src[t.prev..].starts_with(b"xref"));
    }

    #[test]
    fn reads_a_stream_tail() {
        let src = stream_pdf();
        let t = read_tail(&src).unwrap();
        assert!(t.stream);
        assert_eq!(t.size, 5);
        assert_eq!(t.root, "1 0 R");
        assert_eq!(t.id.as_deref(), Some("[<00AB> <00ab>]"));
    }

    #[test]
    fn refuses_what_it_cannot_update() {
        assert_eq!(
            read_tail(b"%PDF-1.4\nno end").unwrap_err(),
            UpdateError::Unreadable("no startxref")
        );
        assert_eq!(
            read_tail(b"%PDF-1.4\nstartxref\n999\n%%EOF\n").unwrap_err(),
            UpdateError::Unreadable("startxref points past the end")
        );
        let mut src = table_pdf();
        let at = src.windows(7).rposition(|w| w == b"trailer").unwrap();
        src.splice(at + 7..at + 7, b"\n<< /Encrypt 9 0 R >>".iter().copied());
        assert_eq!(read_tail(&src).unwrap_err(), UpdateError::Encrypted);
    }

    /// No objects: the source as it was.
    #[test]
    fn an_empty_update_is_the_source() {
        let src = table_pdf();
        assert_eq!(IncrementalUpdate::new(&src).unwrap().finish(), src);
    }

    /// A new object and a new version of the page: the source is the prefix, the offsets are
    /// exact, hayro reads the new page (its new entry) and resolves the new object; a second
    /// update on top of the first works the same and chains `/Prev`.
    #[test]
    fn appends_and_replaces_objects_and_updates_twice() {
        let src = table_pdf();
        let mut up = IncrementalUpdate::new(&src).unwrap();
        let note = up.alloc();
        assert_eq!(note, 5, "numbered after the file's highest");
        up.write(note, 0, b"(first)".to_vec());
        let pdf = Pdf::new(src.clone()).unwrap();
        let page = pdf.pages()[0].raw().clone();
        let mut body = String::from("<<");
        write_entries(&mut body, &page, &[b"MediaBox"]).unwrap();
        let _ = write!(body, " /MediaBox [0 0 300 150] /SPNote {note} 0 R>>");
        up.write(3, 0, body.into_bytes());
        let once = up.finish();
        assert!(once.starts_with(&src), "the source's bytes are kept");
        assert_table_offsets(&once);
        let t1 = read_tail(&once).unwrap();
        assert_eq!(t1.size, 6);
        assert!(
            find(
                &once[t1.prev..],
                format!("/Prev {}", read_tail(&src).unwrap().prev).as_bytes()
            )
            .is_some()
        );
        {
            let pdf = Pdf::new(once.clone()).unwrap();
            let p = &pdf.pages()[0];
            assert_eq!(p.media_box().x1, 300.0);
            let s: hayro_syntax::object::String<'_> =
                pdf.xref().get(ObjectIdentifier::new(5, 0)).unwrap();
            assert_eq!(s.as_bytes(), b"first");
        }

        let mut up = IncrementalUpdate::new(&once).unwrap();
        let second = up.alloc();
        assert_eq!(second, 6);
        up.write(second, 0, stream_object(b"q Q"));
        up.write(note, 0, b"(second)".to_vec());
        let twice = up.finish();
        assert!(twice.starts_with(&once));
        assert_table_offsets(&twice);
        assert!(
            find(
                &twice[read_tail(&twice).unwrap().prev..],
                format!("/Prev {}", t1.prev).as_bytes()
            )
            .is_some()
        );
        let pdf = Pdf::new(twice).unwrap();
        assert_eq!(pdf.pages()[0].media_box().x1, 300.0);
        let s: hayro_syntax::object::String<'_> =
            pdf.xref().get(ObjectIdentifier::new(5, 0)).unwrap();
        assert_eq!(s.as_bytes(), b"second");
        assert!(
            pdf.xref()
                .get::<hayro_syntax::object::Stream<'_>>(ObjectIdentifier::new(6, 0))
                .is_some()
        );
    }

    /// A source with a cross-reference stream gets one too, listing itself, keeping `/ID`; its
    /// new object is read back; and a second update on it works.
    #[test]
    fn updates_a_cross_reference_stream_source() {
        let src = stream_pdf();
        let mut up = IncrementalUpdate::new(&src).unwrap();
        let n = up.alloc();
        assert_eq!(n, 5);
        up.write(n, 0, b"(added)".to_vec());
        let once = up.finish();
        assert!(once.starts_with(&src));
        let t = read_tail(&once).unwrap();
        assert!(t.stream);
        assert_eq!(t.size, 7, "the new object and the stream itself");
        assert_eq!(t.id.as_deref(), Some("[<00AB> <00ab>]"));
        assert!(once[t.prev..].starts_with(b"6 0 obj"));
        assert_stream_offsets(&once);
        let pdf = Pdf::new(once.clone()).unwrap();
        assert_eq!(pdf.pages().len(), 1);
        let s: hayro_syntax::object::String<'_> =
            pdf.xref().get(ObjectIdentifier::new(5, 0)).unwrap();
        assert_eq!(s.as_bytes(), b"added");

        let mut up = IncrementalUpdate::new(&once).unwrap();
        let m = up.alloc();
        assert_eq!(m, 7);
        up.write(m, 0, b"(again)".to_vec());
        let twice = up.finish();
        assert!(twice.starts_with(&once));
        assert_stream_offsets(&twice);
        let pdf = Pdf::new(twice).unwrap();
        let s: hayro_syntax::object::String<'_> =
            pdf.xref().get(ObjectIdentifier::new(7, 0)).unwrap();
        assert_eq!(s.as_bytes(), b"again");
        let s: hayro_syntax::object::String<'_> =
            pdf.xref().get(ObjectIdentifier::new(5, 0)).unwrap();
        assert_eq!(s.as_bytes(), b"added");
    }

    /// The same source and objects give the same bytes.
    #[test]
    fn the_output_is_deterministic() {
        let src = table_pdf();
        let make = || {
            let mut up = IncrementalUpdate::new(&src).unwrap();
            let a = up.alloc();
            let b = up.alloc();
            up.write(b, 0, b"2".to_vec());
            up.write(a, 0, b"1".to_vec());
            up.finish()
        };
        assert_eq!(make(), make());
    }

    #[test]
    fn writes_objects_as_they_read() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(-0.00001), "0");
        assert_eq!(number(1.23456), "1.2346");
        assert_eq!(name(b"A B#"), "/A#20B#23");
        let src = table_pdf();
        let pdf = Pdf::new(src).unwrap();
        let page = pdf.pages()[0].raw().clone();
        let mut s = String::new();
        write_dict(&mut s, &page, &[b"Contents"]).unwrap();
        assert!(s.contains("/Parent 2 0 R"), "{s}");
        assert!(s.contains("/MediaBox [0 0 200 100]"), "{s}");
        assert!(!s.contains("/Contents"), "{s}");
    }
}
