//! Итоговая ведомость: DOCX (альбомный лист, таблица), XLSX и CSV.
//! Файлы собираются вручную (OOXML в ZIP), без внешних библиотек.

use super::core::{Core, Outcome, Work};
use super::json::Json;
use super::zip::ZipWriter;

pub struct Row {
    pub n: usize,
    pub id: String,
    pub file: String,
    pub group: String,
    pub variant: String,
    pub violation: String,
    pub grade: String,
    pub grade_num: Option<f64>,
    pub comment: String,
}

/// Часовой пояс компьютера в минутах (присылает интерфейс: в std его не узнать).
pub static TZ_MIN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(300); // Пермь, UTC+5

/// «23.11.2026 14:05» по местному времени.
pub fn fmt_dt(epoch: u64) -> String {
    let t = epoch as i64 + TZ_MIN.load(std::sync::atomic::Ordering::Relaxed) * 60;
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // дата из числа дней (алгоритм Г. Хиннанта)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:02}.{:02}.{} {:02}:{:02}", d, m, y, secs / 3600, secs % 3600 / 60)
}

fn xml_esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

pub fn violation_label(o: &Outcome) -> String {
    match o.violation.as_str() {
        "ai" => "Да — ИИ".into(),
        "plagiarism" => "Да — списывание".into(),
        _ => {
            if o.status != "done" && o.override_grade.is_none() {
                "—".into()
            } else {
                "Нет".into()
            }
        }
    }
}

pub fn comment_of(o: &Outcome) -> String {
    let mut parts: Vec<String> = Vec::new();
    match o.override_violation.as_str() {
        "ai" => parts.push("Преподаватель отметил использование ИИ.".into()),
        "plagiarism" => parts.push("Преподаватель отметил списывание.".into()),
        "none" if !o.auto_violation.is_empty() => parts.push("Преподаватель снял нарушение, найденное при проверке.".into()),
        _ => {}
    }
    let g = o.final_grade();
    if let Some(g) = g {
        parts.push(format!("Оценка «{}».", super::core::fmt_grade(g)));
    }
    for r in &o.reasons {
        parts.push(format!("{}.", r.trim_end_matches('.')));
    }
    let summary = o.llm.get("summary").str_or("");
    if !summary.trim().is_empty() {
        parts.push(summary.trim().to_string());
    }
    let errs = o.llm.get("errors").str_list();
    if !errs.is_empty() && o.violation.is_empty() {
        parts.push(format!("Замечания: {}.", errs.iter().take(4).map(|e| e.trim_end_matches('.').to_string()).collect::<Vec<_>>().join("; ")));
    }
    if !o.override_comment.trim().is_empty() {
        parts.push(format!("Комментарий преподавателя: {}", o.override_comment.trim()));
    } else if o.override_grade.is_some() && o.override_grade != o.grade {
        parts.push(format!("Оценка изменена преподавателем (была «{}»).", o.grade.map(super::core::fmt_grade).unwrap_or("—".into())));
    }
    let mut ops: Vec<Json> = o.llm.get("opinions").as_arr().to_vec();
    if ops.is_empty() && !o.llm.get("second_opinion").is_null() {
        ops.push(o.llm.get("second_opinion").clone());
    }
    if !ops.is_empty() {
        parts.push(format!(
            "{}: {}.",
            if ops.len() == 1 { "Вторая модель" } else { "Другие модели" },
            ops.iter().map(|x| format!("{} — «{}»", x.get("model").str_or(""), super::core::fmt_grade(x.get("grade").f64_or(0.0)))).collect::<Vec<_>>().join(", ")
        ));
    }
    if o.status == "error" {
        parts.push(format!("Не проверено: {}", o.message));
    }
    parts.join(" ")
}

/// Строки ведомости. `only` — группа или направление («» — все работы).
pub fn rows(core: &Core, only: &str) -> (Vec<Row>, Json) {
    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows = Vec::new();
    let (mut cnt, mut sum, mut ai, mut plag) = (0usize, 0f64, 0usize, 0usize);
    let mut dist = [0usize; 6];
    let mut total = 0usize;
    for w in ws.works.iter() {
        if !only.is_empty() && w.group != only && core.program_of(&w.group) != only {
            continue;
        }
        total += 1;
        let i = rows.len();
        let o = ws.outcomes.get(&w.id).cloned().unwrap_or_default();
        let scan = core.scan(&ws, w);
        let g = o.final_grade();
        if let Some(g) = g {
            cnt += 1;
            sum += g;
            dist[g.floor().clamp(0.0, 5.0) as usize] += 1;
        }
        if o.violation == "ai" {
            ai += 1;
        }
        if o.violation == "plagiarism" {
            plag += 1;
        }
        let (student, in_roster) = core.student_of(&ws, w);
        let review = if o.review.is_empty() { String::new() } else { format!(" Требует внимания преподавателя: {}.", o.review.join("; ").trim_end_matches('.')) };
        rows.push(Row {
            n: i + 1,
            id: w.id.clone(),
            file: if in_roster || !student.starts_with("Вариант") && Some(student.as_str()) != std::path::Path::new(&w.name).file_stem().and_then(|x| x.to_str()) {
                format!("{} ({})", student, w.name)
            } else {
                w.name.clone()
            },
            group: w.group.clone(),
            variant: scan.variant_shown().unwrap_or_else(|| if scan.variant_source == "content" { "не указан".into() } else { "—".into() }),
            violation: violation_label(&o),
            grade: g.map(super::core::fmt_grade).unwrap_or("—".into()),
            grade_num: g,
            comment: format!(
                "{}{}{}",
                comment_of(&o),
                review,
                if o.checked_at > 0 && (o.status == "done" || o.override_grade.is_some()) { format!(" Проверено: {}.", fmt_dt(o.checked_at)) } else { String::new() }
            ),
        });
    }
    let pct = |x: usize| if cnt > 0 { (x as f64 * 100.0 / cnt as f64).round() } else { 0.0 };
    let stats = Json::obj()
        .with("total", total)
        .with("checked", cnt)
        .with("ai", ai)
        .with("plagiarism", plag)
        .with("avg", if cnt > 0 { ((sum as f64 / cnt as f64) * 100.0).round() / 100.0 } else { 0.0 })
        .with("g5", dist[5])
        .with("g4", dist[4])
        .with("g3", dist[3])
        .with("g2", dist[2])
        .with(
            "line",
            format!(
                "Всего работ: {} | Проверено: {} | Использование ИИ: {} | Списывание: {} | «5» — {} ({} %), «4» — {} ({} %), «3» — {} ({} %), «2» — {} ({} %) | Средний балл: {}",
                total,
                cnt,
                ai,
                plag,
                dist[5],
                pct(dist[5]),
                dist[4],
                pct(dist[4]),
                dist[3],
                pct(dist[3]),
                dist[2],
                pct(dist[2]),
                if cnt > 0 { format!("{:.2}", sum as f64 / cnt as f64) } else { "—".into() }
            ),
        );
    (rows, stats)
}

fn principles(core: &Core) -> Vec<String> {
    let st = core.settings();
    let pen = st.get("penalties");
    let mut p = Vec::new();
    if pen.get("enabled").bool_or(true) {
        if pen.get("ai_markers").bool_or(true) || pen.get("ai_model").bool_or(true) {
            p.push("Использование ИИ (сработали скрытые ловушки / вывод нейросети) — автоматическая «2»".to_string());
        }
        if pen.get("ai_multi").bool_or(true) {
            p.push(format!("Найдено {} и более признаков ИИ одновременно — автоматическая «2»", pen.get("ai_multi_count").i64_or(2)));
        }
        if pen.get("injection").bool_or(true) {
            p.push("Попытка повлиять на проверяющую нейросеть (ИИ-инъекция в работе) — автоматическая «2»".into());
        }
        if pen.get("plagiarism").bool_or(true) {
            p.push(format!("Списывание (совпадение текста с другой работой ≥ {} %) — автоматическая «2»", (pen.get("plagiarism_threshold").f64_or(0.6) * 100.0).round()));
        }
    }
    let rules = st.get("rules");
    if rules.get("min_words").get("enabled").bool_or(false) {
        p.push(format!("Меньше {} слов — максимум «{}»", rules.get("min_words").get("words").i64_or(150), rules.get("min_words").get("max").i64_or(3)));
    }
    if rules.get("screenshots").get("enabled").bool_or(false) {
        p.push(format!("Нет скриншотов, которых требует условие — максимум «{}»", rules.get("screenshots").get("max").i64_or(4)));
    }
    if rules.get("wrong_variant").get("enabled").bool_or(false) {
        p.push(format!("Выполнен чужой вариант — максимум «{}»", rules.get("wrong_variant").get("max").i64_or(2)));
    }
    for c in st.get("caps").as_arr() {
        if c.get("enabled").bool_or(true) {
            p.push(c.get("label").str_or(""));
        }
    }
    let v = st.get("variant_missing_cap").i64_or(3);
    if v > 0 {
        p.push(format!("Не указан номер варианта (ни в названии файла, ни в тексте) — максимум «{}»", v));
    }
    if st.get("completeness_rules").bool_or(true) {
        p.push("Сдана половина работы — максимум «3», менее трети — «2»".into());
    }
    p
}

fn title_of(core: &Core, only: &str) -> (String, String) {
    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
    let codes: Vec<String> = ws.sets.iter().map(|s| s.code.clone()).collect();
    let titles: Vec<String> = ws.sets.iter().map(|s| s.title.clone()).collect();
    drop(ws);
    let st = core.settings();
    let title = if codes.is_empty() { "Итоговая ведомость проверки практических работ".to_string() } else { format!("Итоговая ведомость проверки: {}", codes.join(", ")) };
    let mut sub = Vec::new();
    let disc = st.get("discipline").str_or("");
    if !disc.is_empty() {
        sub.push(format!("Дисциплина: «{}»", disc));
    }
    let g = if only.is_empty() { st.get("group_name").str_or("") } else { only.to_string() };
    if !g.is_empty() {
        sub.push(format!("Группа: {}", g));
    }
    if !titles.is_empty() {
        sub.push(format!("Тема: {}", titles.join("; ")));
    }
    let t = st.get("teacher").str_or("");
    if !t.is_empty() {
        sub.push(format!("Преподаватель: {}", t));
    }
    (title, sub.join(" • "))
}

// ------------------------------------------------------------------ DOCX

fn p(text: &str, bold: bool, size_half_pt: u32, align: &str, color: &str) -> String {
    format!(
        "<w:p><w:pPr><w:spacing w:after=\"80\"/>{}</w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:cs=\"Times New Roman\"/>{}<w:sz w:val=\"{}\"/><w:szCs w:val=\"{}\"/>{}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        if align.is_empty() { String::new() } else { format!("<w:jc w:val=\"{}\"/>", align) },
        if bold { "<w:b/>" } else { "" },
        size_half_pt,
        size_half_pt,
        if color.is_empty() { String::new() } else { format!("<w:color w:val=\"{}\"/>", color) },
        xml_esc(text)
    )
}

fn cell(text: &str, width: u32, bold: bool, fill: &str, align: &str) -> String {
    format!(
        "<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>{}<w:vAlign w:val=\"center\"/></w:tcPr><w:p><w:pPr><w:spacing w:after=\"0\"/>{}</w:pPr><w:r><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:cs=\"Times New Roman\"/>{}<w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/></w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:tc>",
        width,
        if fill.is_empty() { String::new() } else { format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{}\"/>", fill) },
        if align.is_empty() { String::new() } else { format!("<w:jc w:val=\"{}\"/>", align) },
        if bold { "<w:b/>" } else { "" },
        xml_esc(text)
    )
}

fn grade_fill(g: Option<f64>) -> &'static str {
    let g = g.map(|x| x.floor() as i64);
    match g {
        Some(5) => "C6EFCE",
        Some(4) => "DDEBF7",
        Some(3) => "FFEB9C",
        Some(2) => "F8CBAD",
        _ => "",
    }
}

pub fn docx(core: &Core, only: &str) -> Vec<u8> {
    let (rows, stats) = rows(core, only);
    let (title, sub) = title_of(core, only);
    let mut body = String::new();
    body.push_str(&p(&title, true, 28, "center", ""));
    if !sub.is_empty() {
        body.push_str(&p(&sub, false, 22, "center", "404040"));
    }
    let pr = principles(core);
    if !pr.is_empty() {
        let txt = pr.iter().enumerate().map(|(i, x)| format!("{}) {}", i + 1, x)).collect::<Vec<_>>().join("; ");
        body.push_str(&p(&format!("Критерии и ограничения: {}.", txt), false, 20, "both", ""));
    }
    body.push_str(&p(&stats.get("line").str_or(""), true, 20, "both", ""));
    // таблица: ширины в twips (альбом A4 ≈ 15400 по ширине текста)
    let widths = [500u32, 2700, 900, 900, 1500, 900, 8000];
    let heads = ["№", "Студент / файл", "Группа", "Вариант", "ИИ / списывание", "Оценка", "Обоснование и замечания"];
    let mut tbl = String::from("<w:tbl><w:tblPr><w:tblW w:w=\"15400\" w:type=\"dxa\"/><w:tblBorders>");
    for b in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        tbl.push_str(&format!("<w:{} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/>", b));
    }
    tbl.push_str("</w:tblBorders><w:tblLayout w:type=\"fixed\"/><w:tblCellMar><w:left w:w=\"70\" w:type=\"dxa\"/><w:right w:w=\"70\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>");
    for w in widths {
        tbl.push_str(&format!("<w:gridCol w:w=\"{}\"/>", w));
    }
    tbl.push_str("</w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr>");
    for (h, w) in heads.iter().zip(widths) {
        tbl.push_str(&cell(h, w, true, "D9E1F2", "center"));
    }
    tbl.push_str("</w:tr>");
    for r in &rows {
        tbl.push_str("<w:tr><w:trPr><w:cantSplit/></w:trPr>");
        tbl.push_str(&cell(&r.n.to_string(), widths[0], false, "", "center"));
        tbl.push_str(&cell(&r.file, widths[1], false, "", ""));
        tbl.push_str(&cell(&r.group, widths[2], false, "", "center"));
        tbl.push_str(&cell(&r.variant, widths[3], false, "", "center"));
        tbl.push_str(&cell(&r.violation, widths[4], r.violation.starts_with("Да"), if r.violation.starts_with("Да") { "FCE4D6" } else { "" }, "center"));
        tbl.push_str(&cell(&r.grade, widths[5], true, grade_fill(r.grade_num), "center"));
        tbl.push_str(&cell(&r.comment, widths[6], false, "", "both"));
        tbl.push_str("</w:tr>");
    }
    tbl.push_str("</w:tbl>");
    body.push_str(&tbl);
    let teacher = core.settings().get("teacher").str_or("");
    body.push_str(&p("", false, 20, "", ""));
    body.push_str(&p(&format!("Дата: {}        Преподаватель: {} ____________________", today(), if teacher.is_empty() { "________________".into() } else { teacher }), false, 22, "", ""));
    body.push_str("<w:sectPr><w:pgSz w:w=\"16838\" w:h=\"11906\" w:orient=\"landscape\"/><w:pgMar w:top=\"850\" w:right=\"720\" w:bottom=\"850\" w:left=\"720\" w:header=\"426\" w:footer=\"426\" w:gutter=\"0\"/></w:sectPr>");

    let doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>{}</w:body></w:document>",
        body
    );
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:eastAsia=\"Times New Roman\" w:cs=\"Times New Roman\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/><w:lang w:val=\"ru-RU\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"80\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style><w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style></w:styles>";
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";
    let doc_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    let core_xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>Проверка заданий</dc:creator></cp:coreProperties>", xml_esc(&title));
    let mut z = ZipWriter::new();
    z.add_str("[Content_Types].xml", ct);
    z.add_str("_rels/.rels", rels);
    z.add_str("word/document.xml", &doc);
    z.add_str("word/styles.xml", styles);
    z.add_str("word/_rels/document.xml.rels", doc_rels);
    z.add_str("docProps/core.xml", &core_xml);
    z.finish()
}

// ------------------------------------------------------------------ XLSX

fn col(i: usize) -> String {
    let mut n = i + 1;
    let mut s = String::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.insert(0, (b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

pub fn xlsx(core: &Core, only: &str) -> Vec<u8> {
    let (rows, stats) = rows(core, only);
    let (title, sub) = title_of(core, only);
    let heads = ["№", "Студент / файл", "Группа", "Вариант", "ИИ / списывание", "Оценка", "Обоснование и замечания"];
    let mut sheet = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"4\" topLeftCell=\"A5\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews><cols><col min=\"1\" max=\"1\" width=\"5\" customWidth=\"1\"/><col min=\"2\" max=\"2\" width=\"38\" customWidth=\"1\"/><col min=\"3\" max=\"4\" width=\"10\" customWidth=\"1\"/><col min=\"5\" max=\"5\" width=\"17\" customWidth=\"1\"/><col min=\"6\" max=\"6\" width=\"9\" customWidth=\"1\"/><col min=\"7\" max=\"7\" width=\"110\" customWidth=\"1\"/></cols><sheetData>");
    let s = |r: usize, c: usize, v: &str, style: u32| format!("<c r=\"{}{}\" t=\"inlineStr\" s=\"{}\"><is><t xml:space=\"preserve\">{}</t></is></c>", col(c), r, style, xml_esc(v));
    let num = |r: usize, c: usize, v: f64, style: u32| format!("<c r=\"{}{}\" s=\"{}\"><v>{}</v></c>", col(c), r, style, v);
    sheet.push_str(&format!("<row r=\"1\">{}</row>", s(1, 0, &title, 1)));
    sheet.push_str(&format!("<row r=\"2\">{}</row>", s(2, 0, &sub, 0)));
    sheet.push_str(&format!("<row r=\"3\">{}</row>", s(3, 0, &stats.get("line").str_or(""), 0)));
    sheet.push_str("<row r=\"4\">");
    for (i, h) in heads.iter().enumerate() {
        sheet.push_str(&s(4, i, h, 2));
    }
    sheet.push_str("</row>");
    for (k, r) in rows.iter().enumerate() {
        let rn = k + 5;
        let gstyle = match r.grade_num.map(|x| x.floor() as i64) {
            Some(5) => 4,
            Some(4) => 5,
            Some(3) => 6,
            Some(2) => 7,
            _ => 3,
        };
        sheet.push_str(&format!("<row r=\"{}\">", rn));
        sheet.push_str(&num(rn, 0, r.n as f64, 3));
        sheet.push_str(&s(rn, 1, &r.file, 3));
        sheet.push_str(&s(rn, 2, &r.group, 3));
        sheet.push_str(&s(rn, 3, &r.variant, 3));
        sheet.push_str(&s(rn, 4, &r.violation, 3));
        match r.grade_num {
            Some(g) => sheet.push_str(&num(rn, 5, g, gstyle)),
            None => sheet.push_str(&s(rn, 5, "—", 3)),
        }
        sheet.push_str(&s(rn, 6, &r.comment, 3));
        sheet.push_str("</row>");
    }
    // порядок частей листа задан схемой OOXML: autoFilter → mergeCells → pageMargins → pageSetup;
    // при другом порядке Excel пишет «ошибка XML» и «восстанавливает» файл
    sheet.push_str(&format!("</sheetData><autoFilter ref=\"A4:G{}\"/><mergeCells count=\"3\"><mergeCell ref=\"A1:G1\"/><mergeCell ref=\"A2:G2\"/><mergeCell ref=\"A3:G3\"/></mergeCells><pageMargins left=\"0.5\" right=\"0.5\" top=\"0.6\" bottom=\"0.6\" header=\"0.3\" footer=\"0.3\"/><pageSetup paperSize=\"9\" orientation=\"landscape\"/></worksheet>", rows.len() + 4));
    let fills = ["C6EFCE", "DDEBF7", "FFEB9C", "F8CBAD"];
    let mut fill_xml = String::from("<fills count=\"7\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FFD9E1F2\"/></patternFill></fill>");
    for f in fills {
        fill_xml.push_str(&format!("<fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{}\"/></patternFill></fill>", f));
    }
    fill_xml.push_str("</fills>");
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><fonts count=\"3\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"14\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>{}<borders count=\"2\"><border><left/><right/><top/><bottom/><diagonal/></border><border><left style=\"thin\"><color rgb=\"FF999999\"/></left><right style=\"thin\"><color rgb=\"FF999999\"/></right><top style=\"thin\"><color rgb=\"FF999999\"/></top><bottom style=\"thin\"><color rgb=\"FF999999\"/></bottom><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"8\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/><xf numFmtId=\"0\" fontId=\"2\" fillId=\"2\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"center\" wrapText=\"1\"/></xf><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"1\" xfId=\"0\" applyBorder=\"1\" applyAlignment=\"1\"><alignment vertical=\"top\" wrapText=\"1\"/></xf><xf numFmtId=\"0\" fontId=\"2\" fillId=\"3\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"top\"/></xf><xf numFmtId=\"0\" fontId=\"2\" fillId=\"4\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"top\"/></xf><xf numFmtId=\"0\" fontId=\"2\" fillId=\"5\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"top\"/></xf><xf numFmtId=\"0\" fontId=\"2\" fillId=\"6\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"top\"/></xf></cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>",
        fill_xml
    );
    let wb = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Ведомость\" sheetId=\"1\" r:id=\"rId1\"/></sheets><definedNames><definedName name=\"_xlnm._FilterDatabase\" localSheetId=\"0\" hidden=\"1\">'Ведомость'!$A$4:$G$4</definedName></definedNames></workbook>";
    let wb_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/></Types>";
    let mut z = ZipWriter::new();
    z.add_str("[Content_Types].xml", ct);
    z.add_str("_rels/.rels", rels);
    z.add_str("xl/workbook.xml", wb);
    z.add_str("xl/_rels/workbook.xml.rels", wb_rels);
    z.add_str("xl/worksheets/sheet1.xml", &sheet);
    z.add_str("xl/styles.xml", &styles);
    z.finish()
}

// ------------------------------------------------------------------ CSV

pub fn csv(core: &Core, only: &str) -> Vec<u8> {
    let (rows, _) = rows(core, only);
    let st = core.settings();
    let sep = st.get("csv_sep").str_or(";").chars().next().unwrap_or(';');
    let mut t: Vec<Vec<String>> = vec![["№", "Студент / файл", "Группа", "Вариант", "ИИ / списывание", "Оценка", "Обоснование"].iter().map(|s| s.to_string()).collect()];
    for r in rows {
        t.push(vec![r.n.to_string(), r.file.clone(), r.group.clone(), r.variant.clone(), r.violation.clone(), r.grade.to_string(), r.comment.clone()]);
    }
    super::moodle::encode_csv(&super::moodle::write_csv(&t, sep), &st.get("csv_encoding").str_or("utf8bom"))
}

fn today() -> String {
    // дата без внешних библиотек, в часовом поясе компьютера (его присылает интерфейс)
    let secs = super::core::now_s() as i64 + TZ_MIN.load(std::sync::atomic::Ordering::Relaxed) * 60;
    let days = secs.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!("{:02}.{:02}.{}", d, m, y)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[allow(dead_code)]
pub fn work_label(w: &Work) -> String {
    format!("{}/{}", w.group, w.name)
}

// ------------------------------------------------------------------ универсальные документы

pub enum Block {
    Title(String),
    Sub(String),
    Heading(String),
    Para(String),
    Bold(String),
    PageBreak,
    /// Абзац из кусков: (текст, скрытый — белый шрифт 1 пт, как ловушки в заданиях)
    Runs(Vec<(String, bool)>),
    Table { heads: Vec<String>, widths: Vec<u32>, rows: Vec<Vec<(String, String)>> }, // (текст, заливка)
}

fn para_lines(text: &str, size: u32) -> String {
    let mut s = String::new();
    for line in text.lines() {
        s.push_str(&p(line, false, size, "both", ""));
    }
    s
}

/// DOCX из блоков. `landscape` — альбомная ориентация.
pub fn docx_blocks(title: &str, blocks: &[Block], landscape: bool) -> Vec<u8> {
    let mut body = String::new();
    for b in blocks {
        match b {
            Block::Title(t) => body.push_str(&p(t, true, 28, "center", "")),
            Block::Sub(t) => body.push_str(&p(t, false, 21, "center", "404040")),
            Block::Heading(t) => body.push_str(&p(t, true, 24, "", "1D2550")),
            Block::Para(t) => body.push_str(&para_lines(t, 22)),
            Block::Bold(t) => body.push_str(&p(t, true, 22, "", "")),
            Block::PageBreak => body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>"),
            Block::Runs(parts) => {
                body.push_str("<w:p><w:pPr><w:spacing w:after=\"80\"/><w:jc w:val=\"both\"/></w:pPr>");
                for (t, hidden) in parts {
                    let rpr = if *hidden {
                        "<w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:cs=\"Times New Roman\"/><w:color w:val=\"FFFFFF\"/><w:sz w:val=\"2\"/><w:szCs w:val=\"2\"/>"
                    } else {
                        "<w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:cs=\"Times New Roman\"/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/>"
                    };
                    body.push_str(&format!("<w:r><w:rPr>{}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", rpr, xml_esc(t)));
                }
                body.push_str("</w:p>");
            }
            Block::Table { heads, widths, rows } => {
                let total: u32 = widths.iter().sum();
                let mut tbl = format!("<w:tbl><w:tblPr><w:tblW w:w=\"{}\" w:type=\"dxa\"/><w:tblBorders>", total);
                for bd in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    tbl.push_str(&format!("<w:{} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/>", bd));
                }
                tbl.push_str("</w:tblBorders><w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>");
                for w in widths {
                    tbl.push_str(&format!("<w:gridCol w:w=\"{}\"/>", w));
                }
                tbl.push_str("</w:tblGrid><w:tr><w:trPr><w:tblHeader/></w:trPr>");
                for (h, w) in heads.iter().zip(widths) {
                    tbl.push_str(&cell(h, *w, true, "D9E1F2", "center"));
                }
                tbl.push_str("</w:tr>");
                for r in rows {
                    tbl.push_str("<w:tr><w:trPr><w:cantSplit/></w:trPr>");
                    for (i, (t, fill)) in r.iter().enumerate() {
                        let w = widths.get(i).copied().unwrap_or(1000);
                        tbl.push_str(&cell(t, w, !fill.is_empty(), fill, if i == 0 { "" } else { "center" }));
                    }
                    tbl.push_str("</w:tr>");
                }
                tbl.push_str("</w:tbl>");
                body.push_str(&tbl);
                body.push_str(&p("", false, 12, "", ""));
            }
        }
    }
    let sect = if landscape {
        "<w:sectPr><w:pgSz w:w=\"16838\" w:h=\"11906\" w:orient=\"landscape\"/><w:pgMar w:top=\"850\" w:right=\"720\" w:bottom=\"850\" w:left=\"720\" w:header=\"426\" w:footer=\"426\" w:gutter=\"0\"/></w:sectPr>"
    } else {
        "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1134\" w:right=\"850\" w:bottom=\"1134\" w:left=\"1418\" w:header=\"709\" w:footer=\"709\" w:gutter=\"0\"/></w:sectPr>"
    };
    body.push_str(sect);
    package_docx(title, &body)
}

fn package_docx(title: &str, body: &str) -> Vec<u8> {
    let doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>{}</w:body></w:document>",
        body
    );
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Times New Roman\" w:hAnsi=\"Times New Roman\" w:eastAsia=\"Times New Roman\" w:cs=\"Times New Roman\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/><w:lang w:val=\"ru-RU\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"80\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style></w:styles>";
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";
    let doc_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    let core_xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>Проверка заданий</dc:creator></cp:coreProperties>", xml_esc(title));
    let mut z = ZipWriter::new();
    z.add_str("[Content_Types].xml", ct);
    z.add_str("_rels/.rels", rels);
    z.add_str("word/document.xml", &doc);
    z.add_str("word/styles.xml", styles);
    z.add_str("word/_rels/document.xml.rels", doc_rels);
    z.add_str("docProps/core.xml", &core_xml);
    z.finish()
}

/// Простой XLSX: заголовок + таблица; значения-числа пишутся как числа, «5»/«4»… раскрашиваются.
pub fn xlsx_table(sheet: &str, title: &str, heads: &[String], rows: &[Vec<String>], widths: &[u32]) -> Vec<u8> {
    let fills: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(c, v)| match v.trim().trim_end_matches('*').parse::<i64>().ok().filter(|g| (2..=5).contains(g) && c >= 2) {
                    Some(5) => "C6EFCE".to_string(),
                    Some(4) => "DDEBF7".to_string(),
                    Some(3) => "FFEB9C".to_string(),
                    Some(_) => "F8CBAD".to_string(),
                    None => String::new(),
                })
                .collect()
        })
        .collect();
    xlsx_table_ex(sheet, title, heads, rows, widths, &fills, 2)
}

/// XLSX с заливкой каждой клетки по `fills` (цвет из четырёх оценочных или ""), `frozen` — сколько столбцов закрепить.
pub fn xlsx_table_ex(sheet: &str, title: &str, heads: &[String], rows: &[Vec<String>], widths: &[u32], fills: &[Vec<String>], frozen: usize) -> Vec<u8> {
    let tl = format!("{}3", col(frozen));
    let mut x = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetViews><sheetView workbookViewId=\"0\"><pane xSplit=\"{}\" ySplit=\"2\" topLeftCell=\"{}\" activePane=\"bottomRight\" state=\"frozen\"/></sheetView></sheetViews><cols>", frozen, tl);
    for (i, w) in widths.iter().enumerate() {
        x.push_str(&format!("<col min=\"{}\" max=\"{}\" width=\"{}\" customWidth=\"1\"/>", i + 1, i + 1, w));
    }
    x.push_str("</cols><sheetData>");
    let s = |r: usize, c: usize, v: &str, style: u32| format!("<c r=\"{}{}\" t=\"inlineStr\" s=\"{}\"><is><t xml:space=\"preserve\">{}</t></is></c>", col(c), r, style, xml_esc(v));
    x.push_str(&format!("<row r=\"1\">{}</row><row r=\"2\">", s(1, 0, title, 1)));
    for (i, h) in heads.iter().enumerate() {
        x.push_str(&s(2, i, h, 2));
    }
    x.push_str("</row>");
    for (k, r) in rows.iter().enumerate() {
        let rn = k + 3;
        x.push_str(&format!("<row r=\"{}\">", rn));
        for (c, v) in r.iter().enumerate() {
            let t = v.trim();
            let st = match fills.get(k).and_then(|f| f.get(c)).map(|f| f.as_str()).unwrap_or("") {
                "C6EFCE" => 4,
                "DDEBF7" => 5,
                "FFEB9C" => 6,
                "F8CBAD" => 7,
                _ => 3,
            };
            // числа («4», «3,5», «12») — числами, с пометками («2*») — текстом
            match t.replace(',', ".").parse::<f64>() {
                Ok(n) if !t.is_empty() && t.len() < 15 => x.push_str(&format!("<c r=\"{}{}\" s=\"{}\"><v>{}</v></c>", col(c), rn, st, n)),
                _ => x.push_str(&s(rn, c, t, st)),
            }
        }
        x.push_str("</row>");
    }
    x.push_str(&format!("</sheetData><mergeCells count=\"1\"><mergeCell ref=\"A1:{}1\"/></mergeCells><pageMargins left=\"0.5\" right=\"0.5\" top=\"0.6\" bottom=\"0.6\" header=\"0.3\" footer=\"0.3\"/><pageSetup paperSize=\"9\" orientation=\"landscape\"/></worksheet>", col(heads.len().max(1) - 1)));
    // стили — как у ведомости
    let src = xlsx_styles();
    let wb = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"{}\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>", xml_esc(sheet));
    let wb_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/></Types>";
    let mut z = ZipWriter::new();
    z.add_str("[Content_Types].xml", ct);
    z.add_str("_rels/.rels", rels);
    z.add_str("xl/workbook.xml", &wb);
    z.add_str("xl/_rels/workbook.xml.rels", wb_rels);
    z.add_str("xl/worksheets/sheet1.xml", &x);
    z.add_str("xl/styles.xml", &src);
    z.finish()
}

fn xlsx_styles() -> String {
    let fills = ["C6EFCE", "DDEBF7", "FFEB9C", "F8CBAD"];
    let mut fill_xml = String::from("<fills count=\"7\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FFD9E1F2\"/></patternFill></fill>");
    for f in fills {
        fill_xml.push_str(&format!("<fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{}\"/></patternFill></fill>", f));
    }
    fill_xml.push_str("</fills>");
    let g = |fill: u32| format!("<xf numFmtId=\"0\" fontId=\"2\" fillId=\"{}\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"top\"/></xf>", fill);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><fonts count=\"3\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"14\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>{}<borders count=\"2\"><border><left/><right/><top/><bottom/><diagonal/></border><border><left style=\"thin\"><color rgb=\"FF999999\"/></left><right style=\"thin\"><color rgb=\"FF999999\"/></right><top style=\"thin\"><color rgb=\"FF999999\"/></top><bottom style=\"thin\"><color rgb=\"FF999999\"/></bottom><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"8\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/><xf numFmtId=\"0\" fontId=\"2\" fillId=\"2\" borderId=\"1\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\" applyAlignment=\"1\"><alignment horizontal=\"center\" vertical=\"center\" wrapText=\"1\"/></xf><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"1\" xfId=\"0\" applyBorder=\"1\" applyAlignment=\"1\"><alignment vertical=\"top\" wrapText=\"1\"/></xf>{}{}{}{}</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>",
        fill_xml,
        g(3),
        g(4),
        g(5),
        g(6)
    )
}

pub fn today_str() -> String {
    today()
}

#[cfg(test)]
mod dt_tests {
    #[test]
    fn date() {
        super::TZ_MIN.store(300, std::sync::atomic::Ordering::Relaxed);
        // 2026-11-23 09:05:00 UTC → 14:05 в Перми
        assert_eq!(super::fmt_dt(1795424700), "23.11.2026 14:05");
    }
}
