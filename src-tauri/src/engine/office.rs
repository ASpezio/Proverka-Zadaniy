//! Excel (.xlsx/.xlsm) и PowerPoint (.pptx/.ppsx): текст для проверки.
//!
//! Для Excel важны не только значения, но и формулы, диаграммы, фильтры —
//! это то, что проверяют в практических по электронным таблицам.
//! Для PowerPoint — текст слайдов, заметки, рисунки, переходы, анимации и
//! гиперссылки (практические по презентациям).

use super::docx::{decode_entities, xml_iter, Extracted, Tok};
use super::zip::ZipArchive;
use std::collections::HashMap;

const MAX_ROWS: usize = 500;

/// Имя тега без префикса пространства имён: «a:t» → «t».
pub fn ln(n: &str) -> &str {
    n.rsplit(':').next().unwrap_or(n)
}

/// Связи из файла .rels: Id → путь цели (относительно папки части).
pub fn rels(zip: &ZipArchive, path: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Some(xml) = zip.read_string(path) else { return m };
    for tok in xml_iter(&xml) {
        if let Tok::Tag(t) = tok {
            if ln(t.name) == "Relationship" {
                if let (Some(id), Some(target)) = (t.attr("Id"), t.attr("Target")) {
                    m.insert(id, target);
                }
            }
        }
    }
    m
}

/// «worksheets/sheet1.xml» относительно «xl/» → «xl/worksheets/sheet1.xml».
pub fn resolve(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|x| !x.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn media_count(zip: &ZipArchive, dir: &str) -> usize {
    zip.names().iter().filter(|n| n.starts_with(dir) && !n.ends_with('/')).count()
}

/// Весь текст из <a:t>/<t> внутри фрагмента XML.
fn texts(xml: &str, tag: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for tok in xml_iter(xml) {
        match tok {
            Tok::Tag(t) if ln(t.name) == tag => inside = !t.closing && !t.self_closing,
            Tok::Text(s) if inside => out.push_str(&decode_entities(s)),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ диаграммы, SmartArt, фигуры

/// Связи части документа: «word/document.xml» → «word/_rels/document.xml.rels».
pub fn part_rels(zip: &ZipArchive, part: &str) -> (String, HashMap<String, String>) {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    (dir.to_string(), rels(zip, &format!("{}/_rels/{}.rels", dir, file)))
}

fn num_fmt(v: &str) -> String {
    match v.trim().parse::<f64>() {
        Ok(x) if x.fract() != 0.0 && v.len() > 8 => {
            let r = (x * 10000.0).round() / 10000.0;
            format!("{}", r)
        }
        _ => v.trim().to_string(),
    }
}

/// Диаграмма Word/PowerPoint/Excel: тип, заголовок, подписи осей и данные рядов
/// (значения берутся из кэша диаграммы — их видно и без исходной таблицы).
pub fn chart_text(xml: &str) -> String {
    #[derive(Default)]
    struct Ser {
        name: String,
        cats: Vec<(usize, String)>,
        vals: Vec<(usize, String)>,
        /// ссылка на ячейки, если значения не сохранены в файле
        val_ref: String,
    }
    let mut kinds: Vec<&str> = Vec::new();
    let mut title = String::new();
    let mut axes: Vec<String> = Vec::new();
    let mut sers: Vec<Ser> = Vec::new();
    let mut cur: Option<Ser> = None;
    let mut section = "";
    let (mut title_depth, mut in_ax, mut in_v, mut in_t, mut in_f) = (0usize, false, false, false, false);
    let mut idx = 0usize;
    let mut buf = String::new();
    let mut ex_texts: Vec<String> = Vec::new(); // диаграммы нового типа (chartEx)
    for tok in xml_iter(xml) {
        match tok {
            Tok::Tag(t) => {
                let n = ln(t.name);
                let open = !t.closing && !t.self_closing;
                let kind = match n {
                    "barChart" => Some("гистограмма (столбчатая/линейчатая)"),
                    "bar3DChart" => Some("объёмная гистограмма"),
                    "lineChart" | "line3DChart" => Some("график"),
                    "pieChart" | "ofPieChart" => Some("круговая"),
                    "pie3DChart" => Some("объёмная круговая"),
                    "doughnutChart" => Some("кольцевая"),
                    "scatterChart" => Some("точечная"),
                    "areaChart" | "area3DChart" => Some("с областями"),
                    "radarChart" => Some("лепестковая"),
                    "bubbleChart" => Some("пузырьковая"),
                    "surfaceChart" | "surface3DChart" => Some("поверхность"),
                    "stockChart" => Some("биржевая"),
                    _ => None,
                };
                if let (Some(k), true) = (kind, open) {
                    if !kinds.contains(&k) {
                        kinds.push(k);
                    }
                }
                if n == "series" && open {
                    if let Some(l) = t.attr("layoutId") {
                        let k = match l.as_str() {
                            "waterfall" => "каскадная",
                            "treemap" => "дерево (treemap)",
                            "sunburst" => "солнечные лучи",
                            "funnel" => "воронка",
                            "boxWhisker" => "ящик с усами",
                            "clusteredColumn" => "гистограмма распределения",
                            "paretoLine" => "Парето",
                            "regionMap" => "карта",
                            _ => "диаграмма нового типа",
                        };
                        if !kinds.contains(&k) {
                            kinds.push(k);
                        }
                    }
                }
                match n {
                    "ser" => {
                        if open {
                            cur = Some(Ser::default());
                        } else if t.closing {
                            if let Some(x) = cur.take() {
                                sers.push(x);
                            }
                        }
                    }
                    "tx" if cur.is_some() && title_depth == 0 => section = if open { "tx" } else { "" },
                    "cat" | "xVal" if cur.is_some() => section = if open { "cat" } else { "" },
                    "val" | "yVal" | "bubbleSize" if cur.is_some() => section = if open && n != "bubbleSize" { "val" } else { "" },
                    "title" if !t.self_closing => {
                        if t.closing {
                            title_depth = title_depth.saturating_sub(1);
                            if title_depth == 0 {
                                let x = buf.split_whitespace().collect::<Vec<_>>().join(" ");
                                if !x.is_empty() {
                                    if in_ax {
                                        axes.push(x);
                                    } else if title.is_empty() && cur.is_none() && sers.is_empty() {
                                        title = x;
                                    }
                                }
                            }
                        } else {
                            title_depth += 1;
                            buf.clear();
                        }
                    }
                    "catAx" | "valAx" | "dateAx" | "serAx" => in_ax = open,
                    "pt" if open => idx = t.attr("idx").and_then(|v| v.parse().ok()).unwrap_or(0),
                    "v" => in_v = open,
                    "t" => in_t = open,
                    "f" => in_f = open,
                    _ => {}
                }
            }
            Tok::Text(x) => {
                let x = decode_entities(x);
                if in_f && section == "val" {
                    if let Some(sr) = cur.as_mut() {
                        sr.val_ref.push_str(&x);
                    }
                    continue;
                }
                if title_depth > 0 && (in_t || in_v) {
                    buf.push_str(&x);
                } else if in_v {
                    match (cur.as_mut(), section) {
                        (Some(sr), "tx") => sr.name.push_str(&x),
                        (Some(sr), "cat") => sr.cats.push((idx, x)),
                        (Some(sr), "val") => sr.vals.push((idx, x)),
                        (None, _) if !x.trim().is_empty() && ex_texts.len() < 200 => ex_texts.push(x),
                        _ => {}
                    }
                }
            }
        }
    }
    let mut out = format!("Диаграмма: {}", if kinds.is_empty() { "тип не определён".to_string() } else { kinds.join(" + ") });
    out.push_str(&if title.is_empty() { ", без заголовка".to_string() } else { format!(", заголовок «{}»", title) });
    if !axes.is_empty() {
        out.push_str(&format!("; подписи осей: {}", axes.iter().map(|a| format!("«{}»", a)).collect::<Vec<_>>().join(", ")));
    }
    if sers.is_empty() && !ex_texts.is_empty() {
        out.push_str(&format!("\nДанные: {}", ex_texts.join("; ")));
    }
    for (i, sr) in sers.iter().enumerate() {
        let name = if sr.name.trim().is_empty() { format!("Ряд {}", i + 1) } else { sr.name.trim().to_string() };
        let pts: Vec<String> = sr
            .vals
            .iter()
            .take(60)
            .map(|(k, v)| match sr.cats.iter().find(|(c, _)| c == k) {
                Some((_, c)) => format!("{} = {}", c.trim(), num_fmt(v)),
                None => num_fmt(v),
            })
            .collect();
        if pts.is_empty() && !sr.val_ref.is_empty() {
            out.push_str(&format!("\n  ряд «{}»: значения из ячеек {}", name, sr.val_ref));
            continue;
        }
        out.push_str(&format!("\n  ряд «{}»: {}{}", name, pts.join("; "), if sr.vals.len() > 60 { " …" } else { "" }));
    }
    out
}

/// SmartArt: узлы схемы с вложенностью (по связям «родитель → потомок»).
pub fn smartart_text(data_xml: &str) -> String {
    let mut nodes: Vec<(String, String, String)> = Vec::new(); // (id, тип, текст)
    let mut cxn: Vec<(String, String, usize)> = Vec::new(); // (родитель, потомок, порядок)
    let mut cur: Option<(String, String, String)> = None;
    let mut in_t = false;
    for tok in xml_iter(data_xml) {
        match tok {
            Tok::Tag(t) => match ln(t.name) {
                "pt" if !t.closing => {
                    let pt = (t.attr("modelId").unwrap_or_default(), t.attr("type").unwrap_or_else(|| "node".into()), String::new());
                    if t.self_closing {
                        nodes.push(pt);
                    } else {
                        cur = Some(pt);
                    }
                }
                "pt" if t.closing => {
                    if let Some(p) = cur.take() {
                        nodes.push(p);
                    }
                }
                "cxn" if !t.closing => {
                    let ty = t.attr("type").unwrap_or_else(|| "parOf".into());
                    if ty == "parOf" {
                        cxn.push((t.attr("srcId").unwrap_or_default(), t.attr("destId").unwrap_or_default(), t.attr("srcOrd").and_then(|v| v.parse().ok()).unwrap_or(0)));
                    }
                }
                "t" => in_t = !t.closing && !t.self_closing,
                "p" if t.closing => {
                    if let Some(c) = cur.as_mut() {
                        if !c.2.is_empty() && !c.2.ends_with(' ') {
                            c.2.push(' ');
                        }
                    }
                }
                _ => {}
            },
            Tok::Text(x) if in_t => {
                if let Some(c) = cur.as_mut() {
                    c.2.push_str(&decode_entities(x));
                }
            }
            _ => {}
        }
    }
    let text_of = |id: &str| nodes.iter().find(|n| n.0 == id).map(|n| n.2.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let root = nodes.iter().find(|n| n.1 == "doc").map(|n| n.0.clone());
    let mut out = String::new();
    fn walk(id: &str, depth: usize, cxn: &[(String, String, usize)], text_of: &dyn Fn(&str) -> String, out: &mut String, guard: &mut usize) {
        *guard += 1;
        if *guard > 500 || depth > 12 {
            return;
        }
        let mut kids: Vec<&(String, String, usize)> = cxn.iter().filter(|c| c.0 == id).collect();
        kids.sort_by_key(|c| c.2);
        for k in kids {
            let t = text_of(&k.1);
            let d = if t.is_empty() { depth } else { depth + 1 };
            if !t.is_empty() {
                // вложенность — значками: строки с отступом при очистке текста выравниваются
                out.push_str(&format!("{} {}\n", if depth == 0 { "•".to_string() } else { "◦".repeat(depth) }, t));
            }
            walk(&k.1, d, cxn, text_of, out, guard);
        }
    }
    let mut guard = 0;
    if let Some(r) = root {
        walk(&r, 0, &cxn, &text_of, &mut out, &mut guard);
    }
    if out.trim().is_empty() {
        for n in nodes.iter().filter(|n| n.1 == "node") {
            let t = n.2.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                out.push_str(&format!("• {}\n", t));
            }
        }
    }
    format!("Схема SmartArt:\n{}", out.trim_end())
}

/// Надписи, фигуры и WordArt на рисунке Excel (xl/drawings/drawingN.xml): текст и картинки.
fn drawing_texts(xml: &str) -> (Vec<String>, usize, Vec<String>, Vec<String>) {
    let mut texts = Vec::new();
    let mut cur = String::new();
    let (mut in_sp, mut in_t) = (0usize, false);
    let mut pics = 0;
    let mut charts = Vec::new();
    let mut dgms = Vec::new();
    for tok in xml_iter(xml) {
        match tok {
            Tok::Tag(t) => match ln(t.name) {
                "sp" => {
                    if t.closing {
                        in_sp = in_sp.saturating_sub(1);
                        if in_sp == 0 {
                            let x = cur.split_whitespace().collect::<Vec<_>>().join(" ");
                            if !x.is_empty() {
                                texts.push(x);
                            }
                            cur.clear();
                        }
                    } else if !t.self_closing {
                        in_sp += 1;
                    }
                }
                "t" => in_t = !t.closing && !t.self_closing,
                "p" if t.closing && in_sp > 0 => cur.push(' '),
                "pic" if !t.closing => pics += 1,
                "chart" if !t.closing => {
                    if let Some(r) = t.attr("r:id") {
                        charts.push(r);
                    }
                }
                "relIds" => {
                    if let Some(r) = t.attr("r:dm") {
                        dgms.push(r);
                    }
                }
                _ => {}
            },
            Tok::Text(x) if in_t && in_sp > 0 => cur.push_str(&decode_entities(x)),
            _ => {}
        }
    }
    (texts, pics, charts, dgms)
}

/// Примечания к ячейкам Excel.
fn comments_text(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut r, mut cur, mut in_t) = (String::new(), String::new(), false);
    for tok in xml_iter(xml) {
        match tok {
            Tok::Tag(t) => match ln(t.name) {
                "comment" if !t.closing => {
                    r = t.attr("ref").unwrap_or_default();
                    cur.clear();
                }
                "comment" if t.closing => {
                    let x = cur.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !x.is_empty() {
                        out.push(format!("{}: {}", r, x));
                    }
                }
                "t" => in_t = !t.closing && !t.self_closing,
                _ => {}
            },
            Tok::Text(x) if in_t => cur.push_str(&decode_entities(x)),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------ Excel: проверка без нейросети

/// Факты о книге Excel для правил «расчёты только формулами» и «нужна диаграмма».
#[derive(Clone, Debug, Default)]
pub struct XlsxFacts {
    pub formulas: usize,
    /// числа, вбитые вручную
    pub constants: usize,
    /// числа без формул среди ячеек, которые в том же столбце считаются формулами: «Лист1!D5:D7»
    pub mixed: Vec<String>,
    /// итоговые строки («Итого», «Сумма», «Среднее»…) с числами без формул
    pub totals: Vec<String>,
    /// виды диаграмм в книге
    pub charts: Vec<String>,
    /// где стоят формулы: «Лист1!D2:D10, E2:E10» (по листам, до 12 диапазонов)
    pub places: Vec<String>,
    /// какие функции встречаются в формулах: SUM, СРЗНАЧ, ЕСЛИ…
    pub funcs: Vec<String>,
}

fn col_num(c: &str) -> usize {
    c.bytes().fold(0, |a, b| a * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize)
}

/// «D», [5,6,7,9] → «D5:D7, D9»
fn ranges(col: &str, rows: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let mut j = i;
        while j + 1 < rows.len() && rows[j + 1] == rows[j] + 1 {
            j += 1;
        }
        out.push(if i == j { format!("{}{}", col, rows[i]) } else { format!("{}{}:{}{}", col, rows[i], col, rows[j]) });
        i = j + 1;
    }
    out.join(", ")
}

pub fn chart_kinds(xml: &str) -> Vec<String> {
    let t = chart_text(xml);
    let head = t.lines().next().unwrap_or("");
    let kinds = head.trim_start_matches("Диаграмма: ").split([',', ';']).next().unwrap_or("").trim().to_string();
    kinds.split(" + ").map(|x| x.to_string()).filter(|x| !x.is_empty() && x != "тип не определён").collect()
}

pub fn xlsx_facts(bytes: &[u8]) -> Option<XlsxFacts> {
    let zip = ZipArchive::open(bytes).ok()?;
    if !zip.names().iter().any(|n| n.starts_with("xl/")) {
        return None;
    }
    let mut f = XlsxFacts::default();
    // общие строки — чтобы узнать подписи «Итого»
    let mut shared: Vec<String> = Vec::new();
    if let Some(xml) = zip.read_string("xl/sharedStrings.xml") {
        let (mut cur, mut in_t) = (String::new(), false);
        for tok in xml_iter(&xml) {
            match tok {
                Tok::Tag(t) if ln(t.name) == "si" && (t.closing || t.self_closing) => shared.push(std::mem::take(&mut cur)),
                Tok::Tag(t) if ln(t.name) == "t" => in_t = !t.closing && !t.self_closing,
                Tok::Text(x) if in_t => cur.push_str(&decode_entities(x)),
                _ => {}
            }
        }
    }
    let wb = zip.read_string("xl/workbook.xml").unwrap_or_default();
    let wb_rels = rels(&zip, "xl/_rels/workbook.xml.rels");
    for tok in xml_iter(&wb) {
        let Tok::Tag(t) = tok else { continue };
        if ln(t.name) != "sheet" {
            continue;
        }
        let name = decode_entities(&t.attr("name").unwrap_or_default());
        let Some(path) = t.attr("r:id").and_then(|r| wb_rels.get(&r)).map(|x| resolve("xl", x)) else { continue };
        let Some(xml) = zip.read_string(&path) else { continue };
        // (столбец, строка) → (формула?, число?, текст)
        let mut cells: Vec<(String, usize, bool, bool, String)> = Vec::new();
        let (mut cref, mut ctype, mut v, mut has_f, mut in_v) = (String::new(), String::new(), String::new(), false, false);
        let (mut in_f, mut ftext) = (false, String::new());
        for tok in xml_iter(&xml) {
            match tok {
                Tok::Tag(t) => match ln(t.name) {
                    "c" if !t.closing => {
                        cref = t.attr("r").unwrap_or_default();
                        ctype = t.attr("t").unwrap_or_default();
                        v.clear();
                        has_f = false;
                        if t.self_closing {
                            cref.clear();
                        }
                    }
                    "c" if t.closing && !cref.is_empty() => {
                        let col: String = cref.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
                        let row: usize = cref[col.len()..].parse().unwrap_or(0);
                        let num = (ctype.is_empty() || ctype == "n") && v.trim().parse::<f64>().is_ok();
                        let text = if ctype == "s" { v.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default() } else if ctype == "inlineStr" || ctype == "str" { v.clone() } else { String::new() };
                        cells.push((col, row, has_f, num, text));
                    }
                    "f" if !t.closing => {
                        has_f = true;
                        in_f = !t.self_closing;
                    }
                    "f" => in_f = false,
                    "v" | "t" => in_v = !t.closing && !t.self_closing,
                    _ => {}
                },
                Tok::Text(x) if in_v => v.push_str(&decode_entities(x)),
                Tok::Text(x) if in_f => {
                    ftext.push_str(&decode_entities(x));
                    ftext.push(' ');
                }
                _ => {}
            }
        }
        // функции формул: «SUM(», «_xlfn.IFS(», «СРЗНАЧ(»
        let chars: Vec<char> = ftext.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_alphabetic() {
                let st = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                    i += 1;
                }
                if i < chars.len() && chars[i] == '(' {
                    let name: String = chars[st..i].iter().collect::<String>().to_uppercase().trim_start_matches("XLFN.").trim_start_matches("XLWS.").to_string();
                    if !name.is_empty() && !f.funcs.contains(&name) && f.funcs.len() < 20 {
                        f.funcs.push(name);
                    }
                }
            } else {
                i += 1;
            }
        }
        // где формулы: по столбцам, диапазонами
        {
            let mut fcols: Vec<String> = cells.iter().filter(|c| c.2).map(|c| c.0.clone()).collect();
            fcols.sort_by_key(|c| col_num(c));
            fcols.dedup();
            let parts: Vec<String> = fcols
                .iter()
                .map(|col| {
                    let mut rs: Vec<usize> = cells.iter().filter(|c| c.2 && &c.0 == col).map(|c| c.1).collect();
                    rs.sort();
                    ranges(col, &rs)
                })
                .collect();
            if !parts.is_empty() {
                let more = parts.len().saturating_sub(12);
                f.places.push(format!("{}!{}{}", name, parts.into_iter().take(12).collect::<Vec<_>>().join(", "), if more > 0 { format!(" и ещё {} столбцов", more) } else { String::new() }));
            }
        }
        f.formulas += cells.iter().filter(|c| c.2).count();
        f.constants += cells.iter().filter(|c| c.3 && !c.2).count();
        // строки итогов: их числа без формул отмечаются отдельно, а не как «столбец»
        let total_rows: Vec<usize> = cells
            .iter()
            .filter(|c| {
                let t = c.4.trim().to_lowercase();
                !t.is_empty() && ["итог", "всего", "сумма", "среднее", "максим", "миним", "total", "average"].iter().any(|k| t.starts_with(k))
            })
            .map(|c| c.1)
            .collect();
        // столбцы, где часть чисел считается формулами, а часть вбита руками
        let mut cols: Vec<String> = cells.iter().map(|c| c.0.clone()).collect();
        cols.sort_by_key(|c| col_num(c));
        cols.dedup();
        for col in &cols {
            let mut run: Vec<(usize, bool)> = cells.iter().filter(|c| &c.0 == col && (c.3 || c.2) && !(total_rows.contains(&c.1) && !c.2)).map(|c| (c.1, c.2)).collect();
            run.sort();
            // непрерывные блоки (пропуск не больше одной строки)
            let mut blocks: Vec<Vec<(usize, bool)>> = Vec::new();
            for x in run {
                match blocks.last_mut() {
                    Some(b) if x.0 <= b.last().map(|l| l.0).unwrap_or(0) + 2 => b.push(x),
                    _ => blocks.push(vec![x]),
                }
            }
            for b in blocks {
                let nf = b.iter().filter(|x| x.1).count();
                let consts: Vec<usize> = b.iter().filter(|x| !x.1).map(|x| x.0).collect();
                // первое значение блока может быть исходным (начальный остаток и т.п.)
                let consts: Vec<usize> = if consts.len() == 1 && consts[0] == b[0].0 { Vec::new() } else { consts };
                if b.len() >= 3 && nf >= 2 && !consts.is_empty() {
                    f.mixed.push(format!("{}!{}", name, ranges(col, &consts)));
                }
            }
        }
        // строки «Итого / Всего / Сумма / Среднее / Максимум…» с числами без формул
        let mut rows: Vec<usize> = cells.iter().map(|c| c.1).collect();
        rows.sort();
        rows.dedup();
        for r in rows {
            let label = cells.iter().filter(|c| c.1 == r && !c.4.is_empty()).map(|c| c.4.to_lowercase()).find(|t| {
                let t = t.trim();
                ["итог", "всего", "сумма", "среднее", "максим", "миним", "total", "average"].iter().any(|k| t.starts_with(k))
            });
            if let Some(l) = label {
                let bad: Vec<String> = cells.iter().filter(|c| c.1 == r && c.3 && !c.2).map(|c| format!("{}{}", c.0, c.1)).collect();
                if !bad.is_empty() {
                    f.totals.push(format!("{}!{} («{}»)", name, bad.join(", "), l.trim().chars().take(30).collect::<String>()));
                }
            }
        }
    }
    for n in zip.names() {
        if n.starts_with("xl/charts/chart") && n.ends_with(".xml") {
            f.charts.extend(chart_kinds(&zip.read_string(n).unwrap_or_default()));
        }
    }
    Some(f)
}

// ------------------------------------------------------------------ Excel

/// Номер столбца из ссылки «AB12» → «AB».
fn col_of(r: &str) -> &str {
    let end = r.find(|c: char| c.is_ascii_digit()).unwrap_or(r.len());
    &r[..end]
}

pub fn from_xlsx(bytes: &[u8]) -> Extracted {
    let zip = match ZipArchive::open(bytes) {
        Ok(z) => z,
        Err(e) => return Extracted { notes: vec![format!("Не удалось открыть Excel-файл: {}", e)], ..Default::default() },
    };
    // общие строки
    let mut shared: Vec<String> = Vec::new();
    if let Some(xml) = zip.read_string("xl/sharedStrings.xml") {
        let mut cur = String::new();
        let mut in_si = false;
        let mut in_t = false;
        let mut in_rph = false; // фонетические подсказки — пропускаем
        for tok in xml_iter(&xml) {
            match tok {
                Tok::Tag(t) if ln(t.name) == "si" => {
                    if t.closing {
                        shared.push(std::mem::take(&mut cur));
                        in_si = false;
                    } else if t.self_closing {
                        shared.push(String::new());
                    } else {
                        in_si = true;
                    }
                }
                Tok::Tag(t) if ln(t.name) == "rPh" => in_rph = !t.closing && !t.self_closing,
                Tok::Tag(t) if ln(t.name) == "t" => in_t = !t.closing && !t.self_closing,
                Tok::Text(s) if in_si && in_t && !in_rph => cur.push_str(&decode_entities(s)),
                _ => {}
            }
        }
    }
    // листы в порядке книги
    let wb = zip.read_string("xl/workbook.xml").unwrap_or_default();
    let wb_rels = rels(&zip, "xl/_rels/workbook.xml.rels");
    let mut sheets: Vec<(String, String, bool)> = Vec::new();
    for tok in xml_iter(&wb) {
        if let Tok::Tag(t) = tok {
            if ln(t.name) == "sheet" {
                let name = decode_entities(&t.attr("name").unwrap_or_default());
                let rid = t.attr("r:id").unwrap_or_default();
                let hidden = t.attr("state").map(|s| s != "visible").unwrap_or(false);
                if let Some(target) = wb_rels.get(&rid) {
                    sheets.push((name, resolve("xl", target), hidden));
                }
            }
        }
    }
    let mut out = String::new();
    let mut notes = Vec::new();
    let mut formulas = 0usize;
    let mut used_charts: Vec<String> = Vec::new();
    for (name, path, hidden) in &sheets {
        let Some(xml) = zip.read_string(path) else { continue };
        out.push_str(&format!("\nЛист «{}»{}:\n", name, if *hidden { " (скрытый лист)" } else { "" }));
        let mut rows = 0usize;
        let mut line: Vec<String> = Vec::new();
        let (mut cref, mut ctype) = (String::new(), String::new());
        let (mut v, mut f, mut inl) = (String::new(), String::new(), String::new());
        let (mut in_v, mut in_f, mut in_is) = (false, false, false);
        let mut features: Vec<&str> = Vec::new();
        let mut merged = 0usize;
        let mut row_no = String::new();
        for tok in xml_iter(&xml) {
            match tok {
                Tok::Tag(t) => match ln(t.name) {
                    "row" if t.closing => {
                        if !line.is_empty() && rows < MAX_ROWS {
                            // «Строка 2 — A: Яблоки | B: 50 | D: =B2*C2 → 150»
                            out.push_str(&format!("Строка {} — ", row_no));
                            out.push_str(&line.join(" | "));
                            out.push('\n');
                        }
                        if !line.is_empty() {
                            rows += 1;
                        }
                        line.clear();
                    }
                    "row" => row_no = t.attr("r").unwrap_or_default(),
                    "c" if !t.closing => {
                        cref = t.attr("r").unwrap_or_default();
                        ctype = t.attr("t").unwrap_or_default();
                        v.clear();
                        f.clear();
                        inl.clear();
                        if t.self_closing {
                            cref.clear();
                        }
                    }
                    "c" if t.closing => {
                        let val = match ctype.as_str() {
                            "s" => v.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                            "inlineStr" => inl.clone(),
                            "b" => (if v.trim() == "1" { "ИСТИНА" } else { "ЛОЖЬ" }).to_string(),
                            "e" => format!("ошибка {}", v.trim()),
                            _ => v.trim().to_string(),
                        };
                        let val = val.replace('\n', " ").trim().to_string();
                        if f.trim() == "(копия формулы)" {
                            formulas += 1;
                            line.push(format!("{}: [формула, скопированная из ячейки выше/левее]{}", col_of(&cref), if val.is_empty() { String::new() } else { format!(" → {}", val) }));
                        } else if !f.trim().is_empty() {
                            formulas += 1;
                            line.push(format!("{}: ={}{}", col_of(&cref), f.trim(), if val.is_empty() { String::new() } else { format!(" → {}", val) }));
                        } else if !val.is_empty() {
                            line.push(format!("{}: {}", col_of(&cref), val));
                        }
                    }
                    "v" => in_v = !t.closing && !t.self_closing,
                    "f" => {
                        in_f = !t.closing && !t.self_closing;
                        // общая формула-копия: <f t="shared" si="0"/> — помечаем
                        if t.self_closing && t.attr("t").as_deref() == Some("shared") {
                            f = "(копия формулы)".into();
                        }
                    }
                    "is" => in_is = !t.closing && !t.self_closing,
                    "mergeCell" => merged += 1,
                    "autoFilter" if !t.closing => features.push("автофильтр"),
                    "conditionalFormatting" if !t.closing => features.push("условное форматирование"),
                    "dataValidation" if !t.closing => features.push("проверка данных"),
                    "pane" => {
                        if t.attr("state").map(|s| s.starts_with("frozen")).unwrap_or(false) {
                            features.push("закреплённые области");
                        }
                    }
                    "sortState" if !t.closing => features.push("сортировка"),
                    "hyperlink" => features.push("гиперссылки"),
                    _ => {}
                },
                Tok::Text(s) => {
                    if in_v {
                        v.push_str(&decode_entities(s));
                    } else if in_f {
                        f.push_str(&decode_entities(s));
                    } else if in_is {
                        inl.push_str(&decode_entities(s));
                    }
                }
            }
        }
        if rows > MAX_ROWS {
            out.push_str(&format!("… ещё строк: {}\n", rows - MAX_ROWS));
        }
        features.sort();
        features.dedup();
        let mut extra: Vec<String> = features.iter().map(|x| x.to_string()).collect();
        if merged > 0 {
            extra.push(format!("объединённых ячеек: {}", merged));
        }
        if !extra.is_empty() {
            out.push_str(&format!("[На листе: {}]\n", extra.join(", ")));
        }
        // рисунок листа: диаграммы, надписи, фигуры, WordArt, SmartArt, картинки; примечания
        let (sdir, srels) = part_rels(&zip, path);
        for target in srels.values() {
            let part = resolve(&sdir, target);
            if part.contains("/drawings/") && part.ends_with(".xml") && !part.contains("vmlDrawing") {
                let Some(dx) = zip.read_string(&part) else { continue };
                let (texts, pics, ch, dg) = drawing_texts(&dx);
                let (ddir, drels) = part_rels(&zip, &part);
                for rid in ch {
                    if let Some(cp) = drels.get(&rid).map(|t| resolve(&ddir, t)) {
                        if let Some(cx) = zip.read_string(&cp) {
                            out.push_str(&chart_text(&cx));
                            out.push('\n');
                            used_charts.push(cp);
                        }
                    }
                }
                for rid in dg {
                    if let Some(dx) = drels.get(&rid).and_then(|t| zip.read_string(&resolve(&ddir, t))) {
                        out.push_str(&smartart_text(&dx));
                        out.push('\n');
                    }
                }
                if !texts.is_empty() {
                    out.push_str(&format!("Надписи и фигуры на листе: {}\n", texts.iter().map(|t| format!("«{}»", t)).collect::<Vec<_>>().join("; ")));
                }
                if pics > 0 {
                    out.push_str(&format!("[Картинок на листе: {}]\n", pics));
                }
            } else if part.contains("comments") && part.ends_with(".xml") && !part.contains("threaded") {
                if let Some(cx) = zip.read_string(&part) {
                    let c = comments_text(&cx);
                    if !c.is_empty() {
                        out.push_str(&format!("Примечания: {}\n", c.join("; ")));
                    }
                }
            }
        }
    }
    // диаграммы на отдельных листах-диаграммах
    let charts: Vec<String> = zip.names().into_iter().filter(|n| n.starts_with("xl/charts/chart") && n.ends_with(".xml") && !used_charts.iter().any(|u| u == n)).map(|n| n.to_string()).collect();
    if !charts.is_empty() {
        out.push_str("\nДиаграммы:\n");
        for c in &charts {
            out.push_str(&chart_text(&zip.read_string(c).unwrap_or_default()));
            out.push('\n');
        }
    }
    if zip.names().iter().any(|n| n.starts_with("xl/pivotTables/")) {
        out.push_str("[В книге есть сводные таблицы]\n");
    }
    if formulas > 0 {
        notes.push(format!("Excel: формул — {}", formulas));
    }
    if sheets.is_empty() {
        notes.push("В Excel-файле не найдено листов".into());
    }
    Extracted { marked: out.trim().to_string(), images: media_count(&zip, "xl/media/"), notes, needs_pdf: false, needs_images: false }
}

// ------------------------------------------------------------------ PowerPoint

/// Текст слайда: абзацы, таблицы (строки через «|»), число рисунков и ссылок.
/// Встроенные в слайд объекты: ('C', rId диаграммы), ('D', rId SmartArt), ('V', видео/звук).
type Embeds = Vec<(char, String)>;

fn slide_text(xml: &str) -> (String, usize, usize, bool, Embeds) {
    let mut embeds: Embeds = Vec::new();
    let mut fallback = 0usize; // mc:Fallback — запасная копия того же объекта
    let mut out = String::new();
    let mut para = String::new();
    let (mut in_t, mut in_tc) = (false, false);
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let (mut pics, mut links) = (0usize, 0usize);
    let mut title_next = false;
    let mut is_title_shape = false;
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
            Tok::Tag(t) => match ln(t.name) {
                "chart" if !t.closing && t.name.starts_with("c:") => {
                    if let Some(r) = t.attr("r:id") {
                        embeds.push(('C', r));
                    }
                }
                "relIds" if !t.closing => {
                    if let Some(r) = t.attr("r:dm") {
                        embeds.push(('D', r));
                    }
                }
                "videoFile" | "audioFile" | "media" if !t.closing => {
                    let what = if ln(t.name) == "audioFile" { "звук" } else { "видео" };
                    if !embeds.iter().any(|e| e.0 == 'V' && e.1 == what) {
                        embeds.push(('V', what.into()));
                    }
                }
                "sp" if !t.closing => is_title_shape = false,
                "ph" => {
                    let ty = t.attr("type").unwrap_or_default();
                    if ty == "title" || ty == "ctrTitle" {
                        is_title_shape = true;
                        title_next = true;
                    }
                }
                "t" => in_t = !t.closing && !t.self_closing,
                "br" => para.push(' '),
                "p" if t.closing => {
                    let p = para.trim().to_string();
                    if !p.is_empty() {
                        if in_tc {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(&p);
                        } else if title_next && is_title_shape {
                            out.push_str(&format!("Заголовок: {}\n", p));
                        } else {
                            out.push_str(&p);
                            out.push('\n');
                        }
                    }
                    para.clear();
                }
                "sp" if t.closing => {
                    if is_title_shape {
                        title_next = false;
                    }
                    is_title_shape = false;
                }
                "tc" => {
                    if t.closing {
                        row.push(std::mem::take(&mut cell));
                        in_tc = false;
                    } else if !t.self_closing {
                        in_tc = true;
                    }
                }
                "tr" if t.closing => {
                    if row.iter().any(|c| !c.trim().is_empty()) {
                        out.push_str(&row.join(" | "));
                        out.push('\n');
                    }
                    row.clear();
                }
                "pic" if !t.closing => pics += 1,
                "hlinkClick" => links += 1,
                _ => {}
            },
            Tok::Text(s) if in_t => para.push_str(&decode_entities(s)),
            _ => {}
        }
    }
    let animated = xml.contains(":timing") && (xml.contains(":anim") || xml.contains("presetClass") || xml.contains(":set>"));
    (out.trim().to_string(), pics, links, animated, embeds)
}

pub fn from_pptx(bytes: &[u8]) -> Extracted {
    let zip = match ZipArchive::open(bytes) {
        Ok(z) => z,
        Err(e) => return Extracted { notes: vec![format!("Не удалось открыть презентацию: {}", e)], ..Default::default() },
    };
    let pres = zip.read_string("ppt/presentation.xml").unwrap_or_default();
    let prels = rels(&zip, "ppt/_rels/presentation.xml.rels");
    let mut order: Vec<String> = Vec::new();
    for tok in xml_iter(&pres) {
        if let Tok::Tag(t) = tok {
            if ln(t.name) == "sldId" {
                if let Some(target) = t.attr("r:id").and_then(|r| prels.get(&r).cloned()) {
                    order.push(resolve("ppt", &target));
                }
            }
        }
    }
    if order.is_empty() {
        // запасной путь: слайды по номерам
        let mut s: Vec<String> = zip.names().into_iter().filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml")).map(|s| s.to_string()).collect();
        s.sort_by_key(|n| n.trim_start_matches("ppt/slides/slide").trim_end_matches(".xml").parse::<usize>().unwrap_or(0));
        order = s;
    }
    let mut out = String::new();
    let (mut tr_count, mut anim_count, mut link_total, mut hidden_n) = (0usize, 0usize, 0usize, 0usize);
    let mut empty_slides = 0usize;
    for (i, path) in order.iter().enumerate() {
        let Some(xml) = zip.read_string(path) else { continue };
        let (text, pics, links, animated, embeds) = slide_text(&xml);
        let mut hidden = false;
        let mut transition = false;
        for tok in xml_iter(&xml) {
            if let Tok::Tag(t) = tok {
                match ln(t.name) {
                    "sld" if !t.closing => hidden = t.attr("show").as_deref() == Some("0"),
                    "transition" if !t.closing => transition = true,
                    _ => {}
                }
            }
        }
        if transition {
            tr_count += 1;
        }
        if animated {
            anim_count += 1;
        }
        if hidden {
            hidden_n += 1;
        }
        link_total += links;
        let mut tags: Vec<String> = Vec::new();
        if pics > 0 {
            tags.push(format!("рисунков: {}", pics));
        }
        if links > 0 {
            tags.push(format!("гиперссылок/кнопок: {}", links));
        }
        if transition {
            tags.push("переход".into());
        }
        if animated {
            tags.push("анимация".into());
        }
        if hidden {
            tags.push("скрытый".into());
        }
        if text.is_empty() && pics == 0 && embeds.is_empty() {
            empty_slides += 1;
        }
        out.push_str(&format!("\nСлайд {}{}:\n", i + 1, if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) }));
        let dir = path.rsplit_once('/').map(|x| x.0).unwrap_or("ppt/slides");
        let file = path.rsplit('/').next().unwrap_or("");
        let srels = rels(&zip, &format!("{}/_rels/{}.rels", dir, file));
        let has_objects = embeds.iter().any(|e| e.0 != 'V');
        out.push_str(if text.is_empty() { if has_objects { "" } else { "(текста нет)" } } else { &text });
        out.push('\n');
        // диаграммы, SmartArt, видео на слайде
        for (k, r) in &embeds {
            match k {
                'C' | 'D' => {
                    if let Some(x) = srels.get(r).and_then(|t| zip.read_string(&resolve(dir, t))) {
                        out.push_str(&if *k == 'C' { chart_text(&x) } else { smartart_text(&x) });
                        out.push('\n');
                    }
                }
                _ => out.push_str(&format!("[На слайде {}]\n", r)),
            }
        }
        // заметки докладчика
        if let Some(n) = srels.values().find(|t| t.contains("notesSlide")) {
            if let Some(nx) = zip.read_string(&resolve(dir, n)) {
                let (nt, _, _, _, _) = slide_text(&nx);
                let nt: String = nt.lines().filter(|l| !l.trim().chars().all(|c| c.is_ascii_digit())).collect::<Vec<_>>().join(" ");
                if !nt.trim().is_empty() {
                    out.push_str(&format!("Заметки: {}\n", nt.trim()));
                }
            }
        }
    }
    let images = media_count(&zip, "ppt/media/");
    let mut summary = vec![format!("слайдов: {}", order.len())];
    summary.push(format!("со сменой слайдов (переходами): {}", tr_count));
    summary.push(format!("с анимацией: {}", anim_count));
    summary.push(format!("гиперссылок и управляющих кнопок: {}", link_total));
    summary.push(format!("изображений в файле: {}", images));
    if hidden_n > 0 {
        summary.push(format!("скрытых слайдов: {}", hidden_n));
    }
    if empty_slides > 0 {
        summary.push(format!("пустых слайдов: {}", empty_slides));
    }
    let head = format!("[Презентация: {}]\n", summary.join(", "));
    let mut notes = Vec::new();
    if order.is_empty() {
        notes.push("В презентации не найдено слайдов".into());
    }
    Extracted { marked: format!("{}{}", head, out.trim()), images, notes, needs_pdf: false, needs_images: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(resolve("xl", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve("ppt/slides", "../notesSlides/notesSlide1.xml"), "ppt/notesSlides/notesSlide1.xml");
        assert_eq!(resolve("xl", "/xl/worksheets/sheet2.xml"), "xl/worksheets/sheet2.xml");
    }
}

#[cfg(test)]
mod rich_tests {
    // ручная проверка на файлах из /tmp/claude-0/t5 (генерируются скриптом)
    #[test]
    #[ignore]
    fn rich_files() {
        for f in ["rich.docx", "chart.pptx", "chart.xlsx"] {
            let p = format!("/tmp/claude-0/t5/{}", f);
            let b = std::fs::read(&p).unwrap();
            let e = super::super::docx::extract(&p, &b);
            println!("===== {} (images {})\n{}\n", f, e.images, e.visible());
        }
    }
}

#[cfg(test)]
mod facts_tests {
    #[test]
    fn ranges_join() {
        assert_eq!(super::ranges("D", &[5, 6, 7, 9]), "D5:D7, D9");
        assert_eq!(super::col_num("AB"), 28);
    }

    /// Формулы не в тех ячейках, что в образце: приложение перечисляет, где они.
    #[test]
    fn formula_places_and_funcs() {
        let mut z = super::super::zip::ZipWriter::new();
        z.add_str("xl/workbook.xml", r#"<workbook xmlns:r="r"><sheets><sheet name="Расчёт" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
        z.add_str("xl/_rels/workbook.xml.rels", r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#);
        z.add_str(
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData>
<row r="2"><c r="B2"><v>5</v></c><c r="C2"><v>3</v></c><c r="F2"><f t="shared" ref="F2:F4" si="0">B2*C2</f><v>15</v></c></row>
<row r="3"><c r="B3"><v>2</v></c><c r="C3"><v>4</v></c><c r="F3"><f t="shared" si="0"/><v>8</v></c></row>
<row r="4"><c r="B4"><v>1</v></c><c r="C4"><v>1</v></c><c r="F4"><f t="shared" si="0"/><v>1</v></c></row>
<row r="6"><c r="H6"><f>_xlfn.IFS(F2&gt;10,"много",TRUE,"мало")</f><v>много</v></c><c r="H7"><f>СУММ(F2:F4)</f><v>24</v></c></row>
</sheetData></worksheet>"#,
        );
        let f = super::xlsx_facts(&z.finish()).unwrap();
        assert_eq!(f.formulas, 5);
        assert_eq!(f.places, vec!["Расчёт!F2:F4, H6:H7".to_string()]);
        assert!(f.funcs.contains(&"IFS".to_string()) && f.funcs.contains(&"СУММ".to_string()), "{:?}", f.funcs);
    }
}
