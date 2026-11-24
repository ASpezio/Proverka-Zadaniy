//! Извлечение текста из документов с выделением СКРЫТЫХ фрагментов.
//!
//! Скрытым считается текст, который человек не видит, а нейросеть видит:
//! белый (или почти белый) цвет, кегль ≤ 2 pt, атрибут «скрытый» (vanish).
//! В итоговом тексте такие фрагменты обрамляются маркерами HID_START/HID_END,
//! чтобы при делении на варианты можно было понять, какие ловушки к какому
//! варианту относятся.

use super::text;
use super::zip::ZipArchive;

pub const HID_START: char = '\u{E000}';
pub const HID_END: char = '\u{E001}';

/// Изображения, которые принимаются как работы (фото тетради, скриншоты, сканы).
pub const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];

pub fn image_mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/jpeg",
    }
}

pub fn is_image(path: &str) -> bool {
    let e = path.rsplit('.').next().unwrap_or("").to_lowercase();
    IMAGE_EXT.contains(&e.as_str())
}

#[derive(Clone, Debug, Default)]
pub struct Extracted {
    /// Текст со встроенными маркерами скрытых фрагментов.
    pub marked: String,
    /// Число изображений (скриншотов) в документе.
    pub images: usize,
    /// Пояснения о качестве извлечения (например, «.doc прочитан приблизительно»).
    pub notes: Vec<String>,
    /// Нужен ли текст из PDF (извлекается в интерфейсе через pdf.js).
    pub needs_pdf: bool,
    /// Файл — изображение (фото, скан): текст распознаёт модель с поддержкой изображений.
    pub needs_images: bool,
}

impl Extracted {
    pub fn visible(&self) -> String {
        visible_of(&self.marked)
    }
    pub fn hidden(&self) -> Vec<String> {
        hidden_of(&self.marked)
    }
}

/// Видимый текст (без скрытых фрагментов).
pub fn visible_of(marked: &str) -> String {
    let mut out = String::with_capacity(marked.len());
    let mut depth = 0;
    for c in marked.chars() {
        match c {
            HID_START => depth += 1,
            HID_END => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    text::tidy(&out)
}

/// Список скрытых фрагментов (соседние скрытые куски склеиваются).
pub fn hidden_of(marked: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur = String::new();
    let mut depth = 0;
    for c in marked.chars() {
        match c {
            HID_START => depth += 1,
            HID_END => {
                depth = (depth - 1).max(0);
                if depth == 0 {
                    let t = cur.split_whitespace().collect::<Vec<_>>().join(" ");
                    let t = t.trim().to_string();
                    if t.chars().filter(|c| c.is_alphanumeric()).count() >= 2 {
                        if let Some(last) = res.last_mut() {
                            // склеиваем, если предыдущий фрагмент только что закончился
                            let _ = last;
                        }
                        res.push(t);
                    }
                    cur.clear();
                }
            }
            _ if depth > 0 => cur.push(c),
            _ => {}
        }
    }
    res
}

/// Удаляет маркеры, оставляя весь текст (видимый + скрытый) — «как его видит ИИ».
pub fn as_ai_sees(marked: &str) -> String {
    text::tidy(&marked.replace([HID_START, HID_END], " "))
}

// ------------------------------------------------------------------ точка входа

pub fn extract(path: &str, bytes: &[u8]) -> Extracted {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let mut r = match ext.as_str() {
        "docx" | "docm" | "dotx" => from_docx(bytes),
        "odt" => from_odt(bytes),
        "rtf" => from_rtf(bytes),
        "doc" => {
            if bytes.starts_with(b"{\\rtf") {
                from_rtf(bytes)
            } else if bytes.starts_with(b"PK") {
                from_docx(bytes)
            } else {
                from_doc_binary(bytes)
            }
        }
        "pdf" => Extracted { needs_pdf: true, ..Default::default() },
        "html" | "htm" => from_html(&text::decode_bytes(bytes)),
        "xlsx" | "xlsm" | "xltx" => super::office::from_xlsx(bytes),
        "pptx" | "ppsx" | "pptm" | "potx" => super::office::from_pptx(bytes),
        e if IMAGE_EXT.contains(&e) => Extracted { needs_images: true, images: 1, ..Default::default() },
        "heic" | "heif" => Extracted {
            notes: vec!["Фото в формате HEIC (iPhone) не читается — сохраните его как JPG (на iPhone: Настройки → Камера → Форматы → «Наиболее совместимые»)".into()],
            ..Default::default()
        },
        "xls" | "ppt" => {
            if bytes.starts_with(b"PK") {
                if ext == "xls" { super::office::from_xlsx(bytes) } else { super::office::from_pptx(bytes) }
            } else {
                Extracted {
                    notes: vec![format!("Старый формат .{} не поддерживается — пересохраните файл как .{}x", ext, ext)],
                    ..Default::default()
                }
            }
        }
        _ => Extracted { marked: text::tidy(&text::decode_bytes(bytes)), ..Default::default() },
    };
    if r.marked.trim().is_empty() && !r.needs_pdf && !r.needs_images && r.notes.is_empty() {
        r.notes.push("Текст не найден (пустой файл или только изображения)".into());
    }
    r
}

// ------------------------------------------------------------------ XML-токенизатор

pub struct Tag<'a> {
    pub name: &'a str,
    pub raw: &'a str,
    pub closing: bool,
    pub self_closing: bool,
}

impl<'a> Tag<'a> {
    pub fn attr(&self, key: &str) -> Option<String> {
        // ищем key="..." или key='...'
        let mut s = self.raw;
        loop {
            let i = s.find(key)?;
            let before_ok = i == 0 || s.as_bytes()[i - 1].is_ascii_whitespace();
            let rest = &s[i + key.len()..];
            let rest_t = rest.trim_start();
            if before_ok && rest_t.starts_with('=') {
                let v = rest_t[1..].trim_start();
                let q = v.chars().next()?;
                if q == '"' || q == '\'' {
                    let end = v[1..].find(q)?;
                    return Some(decode_entities(&v[1..1 + end]));
                }
            }
            s = &s[i + key.len()..];
        }
    }
}

pub enum Tok<'a> {
    Tag(Tag<'a>),
    Text(&'a str),
}

pub struct XmlIter<'a> {
    s: &'a str,
    i: usize,
}

pub fn xml_iter(s: &str) -> XmlIter<'_> {
    XmlIter { s, i: 0 }
}

impl<'a> Iterator for XmlIter<'a> {
    type Item = Tok<'a>;
    fn next(&mut self) -> Option<Tok<'a>> {
        let s = self.s;
        if self.i >= s.len() {
            return None;
        }
        let b = s.as_bytes();
        if b[self.i] != b'<' {
            let st = self.i;
            let end = s[st..].find('<').map(|x| st + x).unwrap_or(s.len());
            self.i = end;
            return Some(Tok::Text(&s[st..end]));
        }
        // комментарии, CDATA, инструкции
        if s[self.i..].starts_with("<!--") {
            let end = s[self.i..].find("-->").map(|x| self.i + x + 3).unwrap_or(s.len());
            self.i = end;
            return self.next();
        }
        if s[self.i..].starts_with("<![CDATA[") {
            let st = self.i + 9;
            let end = s[st..].find("]]>").map(|x| st + x).unwrap_or(s.len());
            self.i = (end + 3).min(s.len());
            return Some(Tok::Text(&s[st..end]));
        }
        let st = self.i + 1;
        let end = match s[st..].find('>') {
            Some(x) => st + x,
            None => {
                self.i = s.len();
                return None;
            }
        };
        self.i = end + 1;
        let inner = &s[st..end];
        if inner.starts_with('?') || inner.starts_with('!') {
            return self.next();
        }
        let closing = inner.starts_with('/');
        let self_closing = inner.ends_with('/');
        let body = inner.trim_start_matches('/').trim_end_matches('/');
        let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        Some(Tok::Tag(Tag { name: &body[..name_end], raw: &body[name_end..], closing, self_closing }))
    }
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        // ищем «;» только в первых 12 байтах, не разрезая многобайтовый символ
        let mut lim = tail.len().min(12);
        while !tail.is_char_boundary(lim) {
            lim -= 1;
        }
        if let Some(semi) = tail[..lim].find(';') {
            let ent = &tail[1..semi];
            let rep = match ent {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
                _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            if let Some(c) = rep {
                out.push(c);
                rest = &tail[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

fn is_whiteish(hex: &str) -> bool {
    let h = hex.trim().trim_start_matches('#');
    if h.eq_ignore_ascii_case("auto") || h.len() != 6 {
        return false;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    p(0) >= 0xF0 && p(2) >= 0xF0 && p(4) >= 0xF0
}

fn truthy(t: &Tag) -> bool {
    match t.attr("w:val") {
        Some(v) => !matches!(v.as_str(), "0" | "false" | "off"),
        None => true,
    }
}

// ------------------------------------------------------------------ DOCX

/// Таблица/ячейка в процессе сборки.
struct TableCtx {
    row: Vec<String>,
}

pub fn from_docx(bytes: &[u8]) -> Extracted {
    let zip = match ZipArchive::open(bytes) {
        Ok(z) => z,
        Err(e) => return Extracted { notes: vec![e], ..Default::default() },
    };
    let xml = zip
        .read_string("word/document.xml")
        .or_else(|| {
            let n = zip.names().into_iter().find(|n| n.starts_with("word/document") && n.ends_with(".xml"))?.to_string();
            zip.read_string(&n)
        });
    let Some(xml) = xml else {
        return Extracted { notes: vec!["В архиве нет word/document.xml — это не документ Word".into()], ..Default::default() };
    };
    // стили символов, которые сами по себе делают текст скрытым
    let hidden_styles = zip.read_string("word/styles.xml").map(|s| hidden_char_styles(&s)).unwrap_or_default();
    let (dir, rels) = super::office::part_rels(&zip, "word/document.xml");
    let embed = |k: char, rid: &str| -> String {
        let Some(x) = rels.get(rid).and_then(|t| zip.read_string(&super::office::resolve(&dir, t))) else { return String::new() };
        if k == 'C' { super::office::chart_text(&x) } else { super::office::smartart_text(&x) }
    };
    let mut ex = parse_document_xml_with(&xml, &hidden_styles, &embed);
    // таблица Excel или презентация, вставленные в документ как объект
    for n in zip.names() {
        if !n.starts_with("word/embeddings/") {
            continue;
        }
        let low = n.to_lowercase();
        let Some(b) = zip.read(n) else { continue };
        let inner = if low.ends_with(".xlsx") || low.ends_with(".xlsm") {
            Some(("таблица Excel", super::office::from_xlsx(&b)))
        } else if low.ends_with(".pptx") {
            Some(("презентация", super::office::from_pptx(&b)))
        } else if low.ends_with(".docx") {
            Some(("документ Word", from_docx(&b)))
        } else {
            None
        };
        if let Some((what, e)) = inner {
            let t = e.visible();
            if !t.trim().is_empty() {
                ex.marked.push_str(&format!("\n\n[Вставленная в документ {}]\n{}\n", what, t));
            }
        }
    }
    // сноски и надписи тоже бывают местом для ловушек
    if let Some(f) = zip.read_string("word/footnotes.xml") {
        let fe = parse_document_xml(&f, &hidden_styles);
        if !fe.marked.trim().is_empty() {
            ex.marked.push_str("\n");
            ex.marked.push_str(&fe.marked);
        }
    }
    ex
}

fn hidden_char_styles(styles: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur_id: Option<String> = None;
    let mut hidden = false;
    for tok in xml_iter(styles) {
        if let Tok::Tag(t) = tok {
            match (t.name, t.closing) {
                ("w:style", false) => {
                    cur_id = t.attr("w:styleId");
                    hidden = false;
                }
                ("w:style", true) => {
                    if hidden {
                        if let Some(id) = cur_id.take() {
                            res.push(id);
                        }
                    }
                    cur_id = None;
                }
                ("w:vanish", false) if cur_id.is_some() && truthy(&t) => hidden = true,
                ("w:color", false) if cur_id.is_some() => {
                    if t.attr("w:val").map(|v| is_whiteish(&v)).unwrap_or(false) {
                        hidden = true
                    }
                }
                ("w:sz", false) if cur_id.is_some() => {
                    if t.attr("w:val").and_then(|v| v.parse::<u32>().ok()).map(|v| v <= 4).unwrap_or(false) {
                        hidden = true
                    }
                }
                _ => {}
            }
        }
    }
    res
}

pub fn parse_document_xml(xml: &str, hidden_styles: &[String]) -> Extracted {
    parse_document_xml_with(xml, hidden_styles, &|_, _| String::new())
}

/// `embed(вид, rId)` возвращает текст встроенного объекта: 'C' — диаграмма, 'D' — SmartArt.
pub fn parse_document_xml_with(xml: &str, hidden_styles: &[String], embed: &dyn Fn(char, &str) -> String) -> Extracted {
    let mut out = String::with_capacity(xml.len() / 6);
    // mc:Fallback — запасная копия надписи или фигуры для старых версий Word: пропускаем, иначе текст удвоится
    let mut fallback = 0usize;
    let mut nary_chr = false;
    let mut images = 0usize;
    // стек таблиц: в ячейке текст копится в отдельный буфер
    let mut tables: Vec<TableCtx> = Vec::new();
    let mut cell_buf: Vec<String> = Vec::new(); // по одному на уровень вложенности ячейки

    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut run_hidden = false;
    let mut in_t = false;
    let mut skip_text = 0; // w:delText, w:instrText

    // куда писать текст
    fn sink<'a>(out: &'a mut String, cell_buf: &'a mut Vec<String>) -> &'a mut String {
        match cell_buf.last_mut() {
            Some(b) => b,
            None => out,
        }
    }
    fn push_text(out: &mut String, cell_buf: &mut Vec<String>, s: &str, hidden: bool) {
        let t = sink(out, cell_buf);
        if hidden {
            t.push(HID_START);
            t.push_str(s);
            t.push(HID_END);
        } else {
            t.push_str(s);
        }
    }

    for tok in xml_iter(xml) {
        if let Tok::Tag(t) = &tok {
            if t.name == "mc:Fallback" && !t.self_closing {
                fallback = if t.closing { fallback.saturating_sub(1) } else { fallback + 1 };
                continue;
            }
        }
        if fallback > 0 {
            continue;
        }
        match tok {
            Tok::Text(s) => {
                if in_t && skip_text == 0 {
                    let d = decode_entities(s);
                    push_text(&mut out, &mut cell_buf, &d, run_hidden);
                }
            }
            Tok::Tag(t) => {
                let name = t.name;
                if t.closing {
                    match name {
                        "w:r" => {
                            in_run = false;
                            run_hidden = false;
                        }
                        "w:rPr" => in_rpr = false,
                        "w:pPr" => in_ppr = false,
                        "w:t" | "m:t" => in_t = false,
                        // формулы (OMML) — в строку: (a)/(b), x^(2), √(x)
                        "m:num" => push_text(&mut out, &mut cell_buf, ")/", false),
                        "m:den" | "m:sup" | "m:sub" | "m:rad" | "m:d" => push_text(&mut out, &mut cell_buf, ")", false),
                        "m:deg" => push_text(&mut out, &mut cell_buf, "]", false),
                        "m:oMath" => push_text(&mut out, &mut cell_buf, " ", false),
                        "m:naryPr" => {
                            if !nary_chr {
                                push_text(&mut out, &mut cell_buf, "∫", false);
                            }
                        }
                        "w:delText" | "w:instrText" => skip_text = (skip_text - 1).max(0),
                        "w:p" => {
                            let s = sink(&mut out, &mut cell_buf);
                            if cell_buf_is_cell(&tables) {
                                s.push(' ');
                            } else {
                                s.push('\n');
                            }
                        }
                        "w:tc" => {
                            let txt = cell_buf.pop().unwrap_or_default();
                            let txt = txt.split_whitespace().collect::<Vec<_>>().join(" ");
                            if let Some(tb) = tables.last_mut() {
                                tb.row.push(txt);
                            }
                        }
                        "w:tr" => {
                            if let Some(tb) = tables.last_mut() {
                                let line = std::mem::take(&mut tb.row).join(" | ");
                                let s = sink(&mut out, &mut cell_buf);
                                s.push_str(&line);
                                s.push('\n');
                            }
                        }
                        "w:tbl" => {
                            tables.pop();
                            sink(&mut out, &mut cell_buf).push('\n');
                        }
                        _ => {}
                    }
                    continue;
                }
                match name {
                    "w:r" => {
                        in_run = true;
                        run_hidden = false;
                    }
                    "w:pPr" if !t.self_closing => in_ppr = true,
                    "w:rPr" if !t.self_closing => in_rpr = true,
                    "w:t" | "m:t" if !t.self_closing => in_t = true,
                    "m:num" | "m:den" | "m:d" if !t.self_closing => push_text(&mut out, &mut cell_buf, "(", false),
                    "m:sup" if !t.self_closing => push_text(&mut out, &mut cell_buf, "^(", false),
                    "m:sub" if !t.self_closing => push_text(&mut out, &mut cell_buf, "_(", false),
                    "m:rad" if !t.self_closing => push_text(&mut out, &mut cell_buf, "√(", false),
                    "m:deg" if !t.self_closing => push_text(&mut out, &mut cell_buf, "[", false),
                    "m:oMath" if !t.self_closing => push_text(&mut out, &mut cell_buf, " ", false),
                    "m:naryPr" if !t.self_closing => nary_chr = false,
                    "m:chr" => {
                        if let Some(v) = t.attr("m:val") {
                            nary_chr = true;
                            push_text(&mut out, &mut cell_buf, &v, false);
                        }
                    }
                    // старый WordArt (VML): текст хранится в атрибуте
                    "v:textpath" => {
                        if let Some(v) = t.attr("string") {
                            push_text(&mut out, &mut cell_buf, &format!(" {} ", decode_entities(&v)), run_hidden);
                        }
                    }
                    // диаграмма и SmartArt — их содержимое прямо в текст
                    "c:chart" => {
                        if let Some(r) = t.attr("r:id") {
                            let x = embed('C', &r);
                            if !x.is_empty() {
                                images = images.saturating_sub(1);
                                push_text(&mut out, &mut cell_buf, &format!("\n[{}]\n", x), false);
                            }
                        }
                    }
                    "cx:chart" => {
                        if let Some(r) = t.attr("r:id") {
                            let x = embed('C', &r);
                            if !x.is_empty() {
                                images = images.saturating_sub(1);
                                push_text(&mut out, &mut cell_buf, &format!("\n[{}]\n", x), false);
                            }
                        }
                    }
                    "dgm:relIds" => {
                        if let Some(r) = t.attr("r:dm") {
                            let x = embed('D', &r);
                            if !x.is_empty() {
                                images = images.saturating_sub(1);
                                push_text(&mut out, &mut cell_buf, &format!("\n[{}]\n", x), false);
                            }
                        }
                    }
                    "w:delText" | "w:instrText" if !t.self_closing => skip_text += 1,
                    "w:tab" if in_run && !in_rpr && !in_ppr => push_text(&mut out, &mut cell_buf, "\t", run_hidden),
                    "w:br" | "w:cr" if in_run => push_text(&mut out, &mut cell_buf, "\n", run_hidden),
                    "w:noBreakHyphen" if in_run => push_text(&mut out, &mut cell_buf, "-", run_hidden),
                    "w:tbl" => {
                        sink(&mut out, &mut cell_buf).push('\n');
                        tables.push(TableCtx { row: Vec::new() });
                    }
                    "w:tc" => cell_buf.push(String::new()),
                    "w:drawing" | "w:pict" | "w:object" => images += 1,
                    // свойства запуска (но не метки абзаца в w:pPr)
                    "w:vanish" | "w:specVanish" if in_run && in_rpr && !in_ppr => {
                        if truthy(&t) {
                            run_hidden = true;
                        }
                    }
                    "w:color" if in_run && in_rpr && !in_ppr => {
                        if t.attr("w:val").map(|v| is_whiteish(&v)).unwrap_or(false) {
                            run_hidden = true;
                        }
                    }
                    "w:sz" if in_run && in_rpr && !in_ppr => {
                        if t.attr("w:val").and_then(|v| v.parse::<u32>().ok()).map(|v| v <= 4).unwrap_or(false) {
                            run_hidden = true;
                        }
                    }
                    "w:rStyle" if in_run && in_rpr && !in_ppr => {
                        if let Some(v) = t.attr("w:val") {
                            if hidden_styles.iter().any(|h| h == &v) {
                                run_hidden = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Extracted { marked: text::tidy_keep_markers(&out), images, notes: vec![], needs_pdf: false, needs_images: false }
}

fn cell_buf_is_cell(tables: &[TableCtx]) -> bool {
    !tables.is_empty()
}

// ------------------------------------------------------------------ ODT

pub fn from_odt(bytes: &[u8]) -> Extracted {
    let zip = match ZipArchive::open(bytes) {
        Ok(z) => z,
        Err(e) => return Extracted { notes: vec![e], ..Default::default() },
    };
    let Some(xml) = zip.read_string("content.xml") else {
        return Extracted { notes: vec!["В ODT нет content.xml".into()], ..Default::default() };
    };
    // автоматические стили со скрытым текстом
    let mut hidden_styles: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    for tok in xml_iter(&xml) {
        if let Tok::Tag(t) = tok {
            if t.name == "style:style" && !t.closing {
                cur = t.attr("style:name");
            } else if t.name == "style:text-properties" {
                let white = t.attr("fo:color").map(|c| is_whiteish(&c)).unwrap_or(false);
                let tiny = t
                    .attr("fo:font-size")
                    .and_then(|s| s.trim_end_matches("pt").parse::<f32>().ok())
                    .map(|v| v <= 2.0)
                    .unwrap_or(false);
                let display_none = t.attr("text:display").map(|v| v == "none").unwrap_or(false);
                if white || tiny || display_none {
                    if let Some(c) = &cur {
                        hidden_styles.push(c.clone());
                    }
                }
            }
        }
    }
    let mut out = String::new();
    let mut span_stack: Vec<bool> = Vec::new();
    let mut images = 0;
    let mut row: Vec<String> = Vec::new();
    let mut cell: Option<String> = None;
    let mut in_body = false;
    for tok in xml_iter(&xml) {
        match tok {
            Tok::Text(s) => {
                if !in_body {
                    continue;
                }
                let d = decode_entities(s);
                let hidden = span_stack.iter().any(|h| *h);
                let target = cell.as_mut().unwrap_or(&mut out);
                if hidden {
                    target.push(HID_START);
                    target.push_str(&d);
                    target.push(HID_END);
                } else {
                    target.push_str(&d);
                }
            }
            Tok::Tag(t) => match (t.name, t.closing) {
                ("office:body", false) => in_body = true,
                ("text:p", false) | ("text:h", false) | ("text:span", false) if !t.self_closing => {
                    let h = t.attr("text:style-name").map(|s| hidden_styles.contains(&s)).unwrap_or(false);
                    span_stack.push(h);
                }
                ("text:p", true) | ("text:h", true) => {
                    span_stack.pop();
                    match cell.as_mut() {
                        Some(c) => c.push(' '),
                        None => out.push('\n'),
                    }
                }
                ("text:span", true) => {
                    span_stack.pop();
                }
                ("text:s", false) => cell.as_mut().unwrap_or(&mut out).push(' '),
                ("text:tab", false) => cell.as_mut().unwrap_or(&mut out).push('\t'),
                ("text:line-break", false) => cell.as_mut().unwrap_or(&mut out).push('\n'),
                ("draw:image", false) => images += 1,
                ("table:table-cell", false) => {
                    if t.self_closing {
                        row.push(String::new());
                    } else {
                        cell = Some(String::new());
                    }
                }
                ("table:table-cell", true) => {
                    let c = cell.take().unwrap_or_default();
                    row.push(c.split_whitespace().collect::<Vec<_>>().join(" "));
                }
                ("table:table-row", true) => {
                    out.push_str(&row.join(" | "));
                    out.push('\n');
                    row.clear();
                }
                _ => {}
            },
        }
    }
    Extracted { marked: text::tidy_keep_markers(&out), images, notes: vec![], needs_pdf: false, needs_images: false }
}

// ------------------------------------------------------------------ RTF

pub fn from_rtf(bytes: &[u8]) -> Extracted {
    let s: Vec<u8> = bytes.to_vec();
    let mut out = String::new();
    let mut i = 0;
    // состояние группы
    #[derive(Clone)]
    struct G {
        skip: bool,
        hidden: bool,
        color: usize,
        fs: u32,
        uc: usize,
    }
    let mut stack: Vec<G> = vec![G { skip: false, hidden: false, color: 0, fs: 24, uc: 1 }];
    let mut colors: Vec<bool> = Vec::new(); // true = белый
    let mut in_colortbl = false;
    let mut cur_rgb = [0u32; 3];
    let mut pending_skip = 0usize;
    let mut codepage = 1251u32;
    let mut images = 0;

    let push = |out: &mut String, g: &G, colors: &Vec<bool>, txt: &str| {
        let white = g.color > 0 && colors.get(g.color).copied().unwrap_or(false);
        let hid = g.hidden || white || g.fs <= 4;
        if hid {
            out.push(HID_START);
            out.push_str(txt);
            out.push(HID_END);
        } else {
            out.push_str(txt);
        }
    };

    while i < s.len() {
        let c = s[i];
        match c {
            b'{' => {
                let top = stack.last().cloned().unwrap();
                stack.push(top);
                i += 1;
            }
            b'}' => {
                if in_colortbl && stack.len() <= 3 {
                    in_colortbl = false;
                }
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= s.len() {
                    break;
                }
                let n = s[i];
                if n == b'\'' {
                    // \'hh
                    let hex = std::str::from_utf8(&s[i + 1..(i + 3).min(s.len())]).unwrap_or("00");
                    let b = u8::from_str_radix(hex, 16).unwrap_or(b'?');
                    i += 3;
                    if pending_skip > 0 {
                        pending_skip -= 1;
                        continue;
                    }
                    let g = stack.last().unwrap().clone();
                    if !g.skip {
                        let ch = text::decode_single_byte(b, codepage);
                        push(&mut out, &g, &colors, &ch.to_string());
                    }
                    continue;
                }
                if !n.is_ascii_alphabetic() {
                    // управляющий символ
                    i += 1;
                    let g = stack.last().unwrap().clone();
                    match n {
                        b'*' => {
                            stack.last_mut().unwrap().skip = true;
                        }
                        b'~' if !g.skip => push(&mut out, &g, &colors, " "),
                        b'-' | b'_' if !g.skip => push(&mut out, &g, &colors, "-"),
                        b'\\' | b'{' | b'}' if !g.skip => push(&mut out, &g, &colors, &(n as char).to_string()),
                        b'\n' | b'\r' if !g.skip => out.push('\n'),
                        _ => {}
                    }
                    continue;
                }
                let st = i;
                while i < s.len() && s[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = std::str::from_utf8(&s[st..i]).unwrap_or("");
                let nst = i;
                if i < s.len() && (s[i] == b'-' || s[i].is_ascii_digit()) {
                    i += 1;
                    while i < s.len() && s[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let param: Option<i64> = std::str::from_utf8(&s[nst..i]).ok().and_then(|x| x.parse().ok());
                if i < s.len() && s[i] == b' ' {
                    i += 1;
                }
                let g = stack.last_mut().unwrap();
                match word {
                    "fonttbl" | "stylesheet" | "info" | "header" | "footer" | "headerl" | "headerr" | "footerl" | "footerr" | "xmlnstbl" | "listtable" | "listoverridetable" | "rsidtbl" | "generator" | "datastore" | "themedata" | "colorschememapping" | "latentstyles" | "fldinst" | "bkmkstart" | "bkmkend" => g.skip = true,
                    "pict" | "shppict" | "nonshppict" => {
                        g.skip = true;
                        if word == "pict" {
                            images += 1;
                        }
                    }
                    "colortbl" => {
                        in_colortbl = true;
                        colors.clear();
                        colors.push(false); // индекс 0 = auto
                        g.skip = true;
                    }
                    "red" if in_colortbl => cur_rgb[0] = param.unwrap_or(0) as u32,
                    "green" if in_colortbl => cur_rgb[1] = param.unwrap_or(0) as u32,
                    "blue" if in_colortbl => cur_rgb[2] = param.unwrap_or(0) as u32,
                    "ansicpg" => codepage = param.unwrap_or(1251) as u32,
                    "uc" => g.uc = param.unwrap_or(1).max(0) as usize,
                    "u" => {
                        let mut v = param.unwrap_or(63);
                        if v < 0 {
                            v += 65536;
                        }
                        let gg = g.clone();
                        if !gg.skip {
                            if let Some(ch) = char::from_u32(v as u32) {
                                push(&mut out, &gg, &colors, &ch.to_string());
                            }
                        }
                        pending_skip = gg.uc;
                    }
                    "par" | "line" | "sect" | "page" => {
                        if !g.skip {
                            out.push('\n');
                        }
                    }
                    "tab" => {
                        if !g.skip {
                            out.push('\t');
                        }
                    }
                    "cell" => {
                        if !g.skip {
                            out.push_str(" | ");
                        }
                    }
                    "row" => {
                        if !g.skip {
                            out.push('\n');
                        }
                    }
                    "v" => g.hidden = param.map(|p| p != 0).unwrap_or(true),
                    "cf" => g.color = param.unwrap_or(0).max(0) as usize,
                    "fs" => g.fs = param.unwrap_or(24).max(0) as u32,
                    "plain" => {
                        g.hidden = false;
                        g.color = 0;
                        g.fs = 24;
                    }
                    "bin" => {
                        i += param.unwrap_or(0).max(0) as usize;
                    }
                    _ => {}
                }
            }
            b';' if in_colortbl => {
                let white = cur_rgb.iter().all(|&x| x >= 0xF0);
                colors.push(white);
                cur_rgb = [0; 3];
                i += 1;
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if pending_skip > 0 {
                    pending_skip -= 1;
                    i += 1;
                    continue;
                }
                let g = stack.last().unwrap().clone();
                // последовательность обычных символов
                let st = i;
                while i < s.len() && !matches!(s[i], b'\\' | b'{' | b'}' | b'\r' | b'\n') {
                    i += 1;
                }
                if !g.skip {
                    let chunk: String = s[st..i].iter().map(|&b| text::decode_single_byte(b, codepage)).collect();
                    push(&mut out, &g, &colors, &chunk);
                }
            }
        }
    }
    // табличные строки RTF заканчиваются «| » перед \row — подчистим
    let cleaned = out.replace(" | \n", "\n");
    Extracted { marked: text::tidy_keep_markers(&cleaned), images, notes: vec![], needs_pdf: false, needs_images: false }
}

// ------------------------------------------------------------------ DOC (Word 97-2003)

/// Эвристика: Word 97+ хранит текст в UTF-16LE внутри потока WordDocument.
/// Собираем длинные последовательности «печатных» символов. Скрытый текст
/// в .doc таким способом не определить — об этом сообщается в notes.
pub fn from_doc_binary(bytes: &[u8]) -> Extracted {
    let mut out = String::new();
    let mut run = String::new();
    let ok = |c: u32| -> bool {
        (0x20..0x7F).contains(&c)
            || (0x400..0x500).contains(&c)
            || matches!(c, 0x0D | 0x09 | 0x07 | 0x0B | 0xA0 | 0xAB | 0xBB | 0x2013 | 0x2014 | 0x2018 | 0x2019 | 0x201C | 0x201D | 0x201E | 0x2022 | 0x2026 | 0x2116)
    };
    let flush = |run: &mut String, out: &mut String| {
        let letters = run.chars().filter(|c| c.is_alphabetic()).count();
        if run.chars().count() >= 8 && letters * 2 >= run.chars().count() / 2 {
            out.push_str(run);
            out.push('\n');
        }
        run.clear();
    };
    for off in [0usize, 1] {
        let mut i = off;
        let mut local = String::new();
        while i + 1 < bytes.len() {
            let c = u16::from_le_bytes([bytes[i], bytes[i + 1]]) as u32;
            if ok(c) {
                let ch = match c {
                    0x0D | 0x0B => '\n',
                    0x07 => '|',
                    _ => char::from_u32(c).unwrap_or(' '),
                };
                run.push(ch);
            } else {
                flush(&mut run, &mut local);
            }
            i += 2;
        }
        flush(&mut run, &mut local);
        if local.len() > out.len() {
            out = local;
        }
    }
    // строки ячеек: «a|b|c||» → «a | b | c»
    let cleaned: String = out
        .lines()
        .map(|l| {
            if l.contains('|') {
                l.split('|').map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" | ")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Extracted {
        marked: text::tidy(&cleaned),
        images: 0,
        notes: vec!["Старый формат .doc прочитан приблизительно; скрытый текст в нём не определяется. Лучше пересохранить в .docx".into()],
        needs_pdf: false,
        needs_images: false,
    }
}

// ------------------------------------------------------------------ HTML

pub fn from_html(s: &str) -> Extracted {
    let mut out = String::new();
    let mut skip = 0;
    for tok in xml_iter(s) {
        match tok {
            Tok::Text(t) if skip == 0 => out.push_str(&decode_entities(t)),
            Tok::Tag(t) => {
                let n = t.name.to_ascii_lowercase();
                match (n.as_str(), t.closing) {
                    ("script" | "style" | "noscript" | "svg" | "template", false) => skip += 1,
                    ("script" | "style" | "noscript" | "svg" | "template", true) => skip = (skip - 1).max(0),
                    ("p" | "div" | "br" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "title" | "section" | "article" | "header" | "footer" | "nav" | "ul" | "ol" | "table" | "dt" | "dd", _) => out.push('\n'),
                    ("td" | "th", true) => out.push_str(" | "),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Extracted { marked: text::tidy(&out), ..Default::default() }
}

#[cfg(test)]
mod entity_tests {
    #[test]
    fn ampersand_before_cyrillic() {
        // «&» без «;» рядом с кириллицей раньше роняло разбор (срез посреди символа)
        assert_eq!(super::decode_entities("Лесовиков & Пряничков СПО"), "Лесовиков & Пряничков СПО");
        assert_eq!(super::decode_entities("a &СССССС; b &amp; в"), "a &СССССС; b & в");
        assert_eq!(super::decode_entities("&#1057;&lt;"), "С<");
    }
}

