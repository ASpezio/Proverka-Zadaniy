//! Минимальный JSON (парсер + сериализатор) без внешних зависимостей.
//! Используется ядром, MCP-протоколом и обменом с интерфейсом.

use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>), // порядок ключей сохраняется
}

impl Default for Json {
    fn default() -> Self {
        Json::Null
    }
}

pub static NULL: Json = Json::Null;

impl Json {
    pub fn obj() -> Json {
        Json::Obj(Vec::new())
    }
    pub fn arr() -> Json {
        Json::Arr(Vec::new())
    }
    /// Добавить/заменить ключ в объекте (builder-стиль).
    pub fn with(mut self, k: &str, v: impl Into<Json>) -> Json {
        self.set(k, v);
        self
    }
    pub fn set(&mut self, k: &str, v: impl Into<Json>) {
        if let Json::Obj(items) = self {
            let v = v.into();
            for it in items.iter_mut() {
                if it.0 == k {
                    it.1 = v;
                    return;
                }
            }
            items.push((k.to_string(), v));
        }
    }
    pub fn push(&mut self, v: impl Into<Json>) {
        if let Json::Arr(a) = self {
            a.push(v.into());
        }
    }
    pub fn get(&self, k: &str) -> &Json {
        if let Json::Obj(items) = self {
            for (kk, v) in items {
                if kk == k {
                    return v;
                }
            }
        }
        &NULL
    }
    pub fn get_mut(&mut self, k: &str) -> Option<&mut Json> {
        if let Json::Obj(items) = self {
            for (kk, v) in items.iter_mut() {
                if kk == k {
                    return Some(v);
                }
            }
        }
        None
    }
    pub fn idx(&self, i: usize) -> &Json {
        if let Json::Arr(a) = self {
            if let Some(v) = a.get(i) {
                return v;
            }
        }
        &NULL
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }
    /// Строка или пусто; числа/булевы приводятся к строке.
    pub fn str_or(&self, def: &str) -> String {
        match self {
            Json::Str(s) => s.clone(),
            Json::Num(n) => fmt_num(*n),
            Json::Bool(b) => b.to_string(),
            _ => def.to_string(),
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            Json::Str(s) => s.trim().replace(',', ".").parse().ok(),
            Json::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    pub fn f64_or(&self, d: f64) -> f64 {
        self.as_f64().unwrap_or(d)
    }
    pub fn i64_or(&self, d: i64) -> i64 {
        self.as_f64().map(|x| x.round() as i64).unwrap_or(d)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) => Some(*n != 0.0),
            Json::Str(s) => {
                let l = s.trim().to_lowercase();
                match l.as_str() {
                    "true" | "да" | "yes" | "1" => Some(true),
                    "false" | "нет" | "no" | "0" => Some(false),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    pub fn bool_or(&self, d: bool) -> bool {
        self.as_bool().unwrap_or(d)
    }
    pub fn as_arr(&self) -> &[Json] {
        if let Json::Arr(a) = self {
            a
        } else {
            &[]
        }
    }
    pub fn as_obj(&self) -> &[(String, Json)] {
        if let Json::Obj(o) = self {
            o
        } else {
            &[]
        }
    }
    pub fn str_list(&self) -> Vec<String> {
        match self {
            Json::Arr(a) => a.iter().map(|x| x.str_or("")).filter(|s| !s.is_empty()).collect(),
            Json::Str(s) if !s.is_empty() => vec![s.clone()],
            _ => vec![],
        }
    }

    pub fn to_string(&self) -> String {
        let mut s = String::new();
        write_json(self, &mut s, None, 0);
        s
    }
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        write_json(self, &mut s, Some(2), 0);
        s
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}
impl From<&String> for Json {
    fn from(s: &String) -> Json {
        Json::Str(s.clone())
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}
impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Num(n)
    }
}
impl From<f32> for Json {
    fn from(n: f32) -> Json {
        Json::Num(n as f64)
    }
}
impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Num(n as f64)
    }
}
impl From<i32> for Json {
    fn from(n: i32) -> Json {
        Json::Num(n as f64)
    }
}
impl From<u32> for Json {
    fn from(n: u32) -> Json {
        Json::Num(n as f64)
    }
}
impl From<u64> for Json {
    fn from(n: u64) -> Json {
        Json::Num(n as f64)
    }
}
impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Num(n as f64)
    }
}
impl From<Vec<Json>> for Json {
    fn from(v: Vec<Json>) -> Json {
        Json::Arr(v)
    }
}
impl From<Vec<String>> for Json {
    fn from(v: Vec<String>) -> Json {
        Json::Arr(v.into_iter().map(Json::Str).collect())
    }
}
impl From<&[String]> for Json {
    fn from(v: &[String]) -> Json {
        Json::Arr(v.iter().cloned().map(Json::Str).collect())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(o: Option<T>) -> Json {
        match o {
            Some(v) => v.into(),
            None => Json::Null,
        }
    }
}
impl From<BTreeMap<String, Json>> for Json {
    fn from(m: BTreeMap<String, Json>) -> Json {
        Json::Obj(m.into_iter().collect())
    }
}

fn fmt_num(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else if n.is_finite() {
        let s = format!("{}", n);
        s
    } else {
        "null".into()
    }
}

pub fn escape_into(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_json(v: &Json, out: &mut String, indent: Option<usize>, level: usize) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&fmt_num(*n)),
        Json::Str(s) => escape_into(s, out),
        Json::Arr(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Some(n) = indent {
                    out.push('\n');
                    out.push_str(&" ".repeat(n * (level + 1)));
                }
                write_json(x, out, indent, level + 1);
            }
            if let Some(n) = indent {
                out.push('\n');
                out.push_str(&" ".repeat(n * level));
            }
            out.push(']');
        }
        Json::Obj(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Some(n) = indent {
                    out.push('\n');
                    out.push_str(&" ".repeat(n * (level + 1)));
                }
                escape_into(k, out);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_json(x, out, indent, level + 1);
            }
            if let Some(n) = indent {
                out.push('\n');
                out.push_str(&" ".repeat(n * level));
            }
            out.push('}');
        }
    }
}

// ---------------------------------------------------------------- парсер

pub fn parse(s: &str) -> Result<Json, String> {
    let mut p = Parser { b: s.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("лишние символы после JSON в позиции {}", p.i));
    }
    Ok(v)
}

/// Парсинг с допуском: находит первый корректный JSON-объект в тексте
/// (ответы моделей часто обёрнуты в ```json ... ``` или содержат пояснения).
pub fn extract_object(text: &str) -> Option<Json> {
    let t = strip_think(text);
    if let Ok(v) = parse(t.trim()) {
        if matches!(v, Json::Obj(_)) {
            return Some(v);
        }
    }
    let bytes = t.as_bytes();
    let mut best: Option<Json> = None;
    let mut start = 0;
    while let Some(off) = t[start..].find('{') {
        let st = start + off;
        // ищем парную скобку с учётом строк
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        let mut end = None;
        for (j, &c) in bytes[st..].iter().enumerate() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(st + j + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => {
                let cand = &t[st..e];
                if let Ok(v) = parse(cand).or_else(|_| parse(&repair(cand))) {
                    let n = v.as_obj().len();
                    if best.as_ref().map(|b| b.as_obj().len() < n).unwrap_or(true) {
                        best = Some(v);
                    }
                }
                start = st + 1;
            }
            None => {
                // незакрытый объект (ответ оборван) — пробуем починить
                let cand = &t[st..];
                if let Ok(v) = parse(&repair_truncated(cand)) {
                    if best.is_none() {
                        best = Some(v);
                    }
                }
                break;
            }
        }
        if best.as_ref().map(|b| b.as_obj().len() >= 3).unwrap_or(false) {
            break;
        }
    }
    best
}

/// Убирает блоки рассуждений <think>…</think> моделей-«размышлялок».
pub fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    loop {
        let Some(a) = s.find("<think>") else { break };
        match s[a..].find("</think>") {
            Some(b) => s.replace_range(a..a + b + 8, ""),
            None => {
                s.truncate(a);
                break;
            }
        }
    }
    if let Some(b) = s.find("</think>") {
        s = s[b + 8..].to_string();
    }
    s
}

/// Мелкий ремонт: висячие запятые, «умные» кавычки вокруг ключей.
fn repair(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            } else if c == '\n' {
                out.pop();
                out.push_str("\\n");
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == ',' {
            // висячая запятая перед } или ]
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Закрывает оборванный JSON (если модель упёрлась в лимит токенов).
fn repair_truncated(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    let mut out = String::new();
    for c in s.chars() {
        out.push(c);
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_str {
        out.push('"');
    }
    let trimmed = out.trim_end().trim_end_matches(',').to_string();
    let mut out = trimmed;
    // если оборвалось после ключа без значения
    if out.ends_with(':') {
        out.push_str("null");
    }
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    repair(&out)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("JSON: {} (позиция {})", m, self.i))
    }
    fn value(&mut self) -> Result<Json, String> {
        if self.i >= self.b.len() {
            return self.err("неожиданный конец");
        }
        self.depth += 1;
        if self.depth > 200 {
            return self.err("слишком глубокая вложенность");
        }
        let r = match self.b[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => self.string().map(Json::Str),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("неожиданный символ"),
        };
        self.depth -= 1;
        r
    }
    fn lit(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            self.err("неизвестный литерал")
        }
    }
    fn number(&mut self) -> Result<Json, String> {
        let st = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[st..self.i]).unwrap_or("");
        s.parse::<f64>().map(Json::Num).or_else(|_| self.err("плохое число"))
    }
    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return self.err("обрыв \\u");
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
        let v = u32::from_str_radix(s, 16).map_err(|_| "плохой \\u".to_string())?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // "
        let mut out: Vec<u8> = Vec::new();
        loop {
            if self.i >= self.b.len() {
                return self.err("незакрытая строка");
            }
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return self.err("обрыв экранирования");
                    }
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => out.push(e),
                    }
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
    fn array(&mut self) -> Result<Json, String> {
        self.i += 1;
        let mut v = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b']' {
            self.i += 1;
            return Ok(Json::Arr(v));
        }
        loop {
            self.ws();
            v.push(self.value()?);
            self.ws();
            if self.i >= self.b.len() {
                return self.err("незакрытый массив");
            }
            match self.b[self.i] {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    break;
                }
                _ => return self.err("ожидалась , или ]"),
            }
        }
        Ok(Json::Arr(v))
    }
    fn object(&mut self) -> Result<Json, String> {
        self.i += 1;
        let mut o: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            return Ok(Json::Obj(o));
        }
        loop {
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b'"' {
                return self.err("ожидался ключ-строка");
            }
            let k = self.string()?;
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return self.err("ожидалось :");
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            if let Some(slot) = o.iter_mut().find(|x| x.0 == k) {
                slot.1 = v;
            } else {
                o.push((k, v));
            }
            self.ws();
            if self.i >= self.b.len() {
                return self.err("незакрытый объект");
            }
            match self.b[self.i] {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    break;
                }
                _ => return self.err("ожидалась , или }"),
            }
        }
        Ok(Json::Obj(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let s = r#"{"a":[1,2.5,"x\nЖ"],"b":{"c":null,"d":true}}"#;
        let v = parse(s).unwrap();
        assert_eq!(v.get("a").idx(2).as_str(), Some("x\nЖ"));
        assert_eq!(parse(&v.to_string()).unwrap(), v);
    }
    #[test]
    fn extract() {
        let t = "<think>hmm {no}</think>Вот ответ:\n```json\n{\"grade\": 4, \"tasks\": [1,2,],}\n```";
        let v = extract_object(t).unwrap();
        assert_eq!(v.get("grade").i64_or(0), 4);
        let t2 = "{\"grade\": 3, \"summary\": \"оборвано";
        assert_eq!(extract_object(t2).unwrap().get("grade").i64_or(0), 3);
    }
}
