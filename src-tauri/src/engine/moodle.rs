//! Moodle: импорт ответов из архива «Скачать все ответы» и заполнение
//! таблицы оценок («Скачать таблицу оценок» → «Загрузить таблицу оценок»).
//!
//! Имена в архиве Moodle: «Лесовиков Иван_123456_assignsubmission_file_Работа.docx»
//! (или папка «Лесовиков Иван_123456_assignsubmission_file» с файлами внутри),
//! текст ответа: «…_assignsubmission_onlinetext_onlinetext.html».

use super::zip::ZipArchive;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Ответ одного студента.
#[derive(Clone, Debug)]
pub struct Submission {
    pub student: String,
    pub participant: String,
    /// главный файл (его и проверяем)
    pub main: String,
    /// остальные файлы студента
    pub extra: Vec<String>,
}

/// «Лесовиков Иван_123456_assignsubmission_file_Работа.docx» → («Лесовиков Иван», «123456»).
pub fn parse_name(s: &str) -> Option<(String, String)> {
    let i = s.find("_assignsubmission_")?;
    let before = &s[..i];
    let (name, id) = before.rsplit_once('_')?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let name = name.trim().split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    Some((name, id.to_string()))
}

fn safe_component(s: &str) -> String {
    let t: String = s.chars().map(|c| if "<>:\"|?*\\".contains(c) || (c as u32) < 32 { '_' } else { c }).collect();
    let t = t.trim().trim_end_matches('.').to_string();
    if t.is_empty() {
        "_".into()
    } else {
        t
    }
}

/// Распаковывает zip в папку `dest`. Пути внутри архива проверяются:
/// «..», абсолютные пути и диски не допускаются.
pub fn unzip(zip_path: &str, dest: &Path) -> Result<usize, String> {
    let data = std::fs::read(zip_path).map_err(|e| format!("не удалось открыть архив: {}", e))?;
    let z = ZipArchive::open(&data)?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut n = 0;
    for name in z.names() {
        if name.ends_with('/') {
            continue;
        }
        let mut rel = PathBuf::new();
        let mut ok = true;
        for c in Path::new(&name.replace('\\', "/")).components() {
            match c {
                Component::Normal(x) => rel.push(safe_component(&x.to_string_lossy())),
                Component::CurDir => {}
                _ => ok = false,
            }
        }
        if !ok || rel.as_os_str().is_empty() {
            continue;
        }
        let Some(bytes) = z.read(name) else { continue };
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&out, bytes).is_ok() {
            n += 1;
            // время сдачи из архива: по нему видно, кто сдал работу первым
            let t = z.mtime(name);
            if t > 0 {
                if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&out) {
                    let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t));
                }
            }
        }
    }
    Ok(n)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else {
            out.push(p);
        }
    }
}

/// Приоритет формата при выборе главного файла.
fn rank(path: &str) -> i32 {
    let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "docx" | "docm" => 0,
        "odt" => 1,
        "doc" => 2,
        "rtf" => 3,
        "pdf" => 4,
        "xlsx" | "xlsm" | "pptx" | "ppsx" | "pptm" => 5,
        "txt" | "md" => 6,
        "html" | "htm" => 7,
        "jpg" | "jpeg" | "png" | "webp" => 8,
        _ => 9,
    }
}

/// Находит ответы студентов в распакованной папке Moodle.
pub fn collect(root: &Path, supported: &[&str]) -> (Vec<Submission>, Vec<String>) {
    let mut files = Vec::new();
    walk(root, &mut files, 0);
    let mut by: HashMap<String, (String, Vec<String>)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut skipped = Vec::new();
    for f in files {
        let rel = f.strip_prefix(root).unwrap_or(&f);
        // имя студента — в имени файла или одной из папок
        let found = rel.components().rev().filter_map(|c| match c {
            Component::Normal(x) => parse_name(&x.to_string_lossy()),
            _ => None,
        }).next();
        let path = f.to_string_lossy().to_string();
        let ext = f.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Some((student, id)) = found else {
            skipped.push(rel.to_string_lossy().to_string());
            continue;
        };
        let key = format!("{}|{}", student, id);
        if !by.contains_key(&key) {
            order.push(key.clone());
        }
        let e = by.entry(key).or_insert_with(|| (student.clone(), Vec::new()));
        if supported.contains(&ext.as_str()) {
            e.1.push(path);
        } else {
            skipped.push(rel.to_string_lossy().to_string());
        }
    }
    let mut subs = Vec::new();
    for key in order {
        let (student, mut paths) = by.remove(&key).unwrap_or_default();
        if paths.is_empty() {
            continue;
        }
        let participant = key.rsplit('|').next().unwrap_or("").to_string();
        // главный — документ «старшего» формата, из них самый большой;
        // текст ответа (onlinetext) берём, только если файлов нет
        paths.sort_by_key(|p| (rank(p), std::cmp::Reverse(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))));
        // только фото — страницы по порядку имён («стр1», «стр2»…)
        if rank(&paths[0]) == 8 {
            paths.sort_by_key(|p| (rank(p), p.to_lowercase()));
        }
        let main = paths.remove(0);
        subs.push(Submission { student, participant, main, extra: paths });
    }
    subs.sort_by(|a, b| a.student.cmp(&b.student));
    (subs, skipped)
}

// ------------------------------------------------------------------ таблица оценок (CSV)

/// Разбор CSV с кавычками (RFC 4180), разделитель — запятая или точка с запятой.
pub fn parse_csv(text: &str) -> (Vec<Vec<String>>, char) {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("");
    let sep = if first.matches(';').count() > first.matches(',').count() { ';' } else { ',' };
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut q = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if q {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    cell.push('"');
                    it.next();
                } else {
                    q = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' {
            q = true;
        } else if c == sep {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            row.push(std::mem::take(&mut cell));
            if !(row.len() == 1 && row[0].is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    (rows, sep)
}

pub fn write_csv(rows: &[Vec<String>], sep: char) -> String {
    let mut s = String::from("\u{feff}");
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .map(|c| {
                if c.contains(sep) || c.contains('"') || c.contains('\n') || c.contains('\r') {
                    format!("\"{}\"", c.replace('"', "\"\""))
                } else {
                    c.clone()
                }
            })
            .collect();
        s.push_str(&line.join(&sep.to_string()));
        s.push_str("\r\n");
    }
    s
}

/// CSV-текст (из `write_csv`) → байты в нужной кодировке:
/// `utf8bom` — UTF-8 с BOM (по умолчанию: Moodle его понимает, Excel открывает без «каракулей»),
/// `utf8` — UTF-8 без BOM, `cp1251` — Windows-1251.
pub fn encode_csv(text: &str, enc: &str) -> Vec<u8> {
    let body = text.trim_start_matches('\u{feff}');
    match enc {
        "utf8" => body.as_bytes().to_vec(),
        "cp1251" => super::text::encode_cp1251(body),
        _ => {
            let mut b = vec![0xEF, 0xBB, 0xBF];
            b.extend_from_slice(body.as_bytes());
            b
        }
    }
}

/// Ключ имени для сравнения: «Лесовиков  Иван Ё.» → «иван лесовиков е» (слова по алфавиту).
pub fn name_key(s: &str) -> String {
    let mut w: Vec<String> = s
        .to_lowercase()
        .replace('ё', "е")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|x| x.chars().count() > 1)
        .map(|x| x.to_string())
        .collect();
    w.sort();
    w.join(" ")
}

/// Один и тот же человек: все слова короткой записи есть в длинной
/// («Лесовиков Иван» и «Лесовиков Иван Иванович»).
pub fn same_person(a: &str, b: &str) -> bool {
    let ka = name_key(a);
    let kb = name_key(b);
    if ka.is_empty() || kb.is_empty() {
        return false;
    }
    let wa: Vec<&str> = ka.split(' ').collect();
    let wb: Vec<&str> = kb.split(' ').collect();
    let (short, long) = if wa.len() <= wb.len() { (&wa, &wb) } else { (&wb, &wa) };
    short.len() >= 2 && short.iter().all(|x| long.contains(x)) || (short.len() == 1 && long.len() == 1 && short[0] == long[0])
}

/// Номер столбца по одному из названий (рус./англ. интерфейс Moodle).
pub fn col(header: &[String], names: &[&str]) -> Option<usize> {
    header.iter().position(|h| {
        let h = h.trim().trim_start_matches('\u{feff}').to_lowercase();
        names.iter().any(|n| h == *n || h.starts_with(n))
    })
}


// ------------------------------------------------------------------ оценки из Moodle

/// Строки таблицы из файла: CSV (как его отдаёт Moodle) или Excel (первый лист).
pub fn table_rows(path: &str, bytes: &[u8]) -> Vec<Vec<String>> {
    let low = path.to_lowercase();
    if low.ends_with(".xlsx") || low.ends_with(".xls") && bytes.starts_with(b"PK") {
        // текст листа приложение читает как «Строка N — A: … | B: …»
        let text = super::office::from_xlsx(bytes).marked;
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut sheets = 0;
        for line in text.lines() {
            if line.starts_with("Лист «") {
                sheets += 1;
                if sheets > 1 {
                    break;
                }
                continue;
            }
            let Some(rest) = line.strip_prefix("Строка ") else { continue };
            let Some(p) = rest.find(" — ") else { continue };
            let mut row: Vec<String> = Vec::new();
            for cell in rest[p + " — ".len()..].split(" | ") {
                let Some(c) = cell.find(": ") else { continue };
                let letters = &cell[..c];
                if letters.is_empty() || !letters.chars().all(|x| x.is_ascii_uppercase()) {
                    continue;
                }
                let idx = letters.chars().fold(0usize, |a, x| a * 26 + (x as usize - 'A' as usize + 1)) - 1;
                while row.len() <= idx {
                    row.push(String::new());
                }
                row[idx] = cell[c + 2..].trim().to_string();
            }
            rows.push(row);
        }
        return rows;
    }
    parse_csv(&super::text::decode_bytes(bytes)).0
}

/// Число из ячейки Moodle: «4,50», «85.00 %», «-» → None.
pub fn grade_value(s: &str) -> Option<f64> {
    let t: String = s.trim().replace(',', ".").chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

/// Перевод оценки Moodle в пятибалльную: шкала до 5 — как есть, иначе пропорционально
/// (85 из 100 → 4,25 → «4», с половинками — «4,5» по общему правилу округления приложения).
pub fn to_five(v: f64, max: f64) -> f64 {
    let g = if max > 5.5 { v / max * 5.0 } else { v };
    g.clamp(0.0, 5.0)
}

/// Столбец журнала оценок Moodle: «Тест: Тестирование по разделу 1 (Проценты)» →
/// (код, полное название, вид work|test, тип: value|percent|letter|feedback).
pub fn column_info(h: &str) -> Option<(String, String, &'static str, &'static str)> {
    let low = h.to_lowercase();
    // итог курса, который считает сам Moodle, — свой столбец: в средние журнала не идёт
    if low.trim_start().starts_with("итоговая оценка за курс") || low.trim_start().starts_with("course total") {
        let typ = if low.contains("(процент") || low.contains("(percent") {
            "percent"
        } else if low.contains("(букв") || low.contains("(letter") {
            "letter"
        } else if low.contains("(отзыв") || low.contains("(feedback") {
            "feedback"
        } else {
            "value"
        };
        return Some(("Итог курса".into(), "Итоговая оценка за курс (Moodle)".into(), "custom", typ));
    }
    let kinds: [(&str, &str); 12] = [
        ("задание:", "work"),
        ("assignment:", "work"),
        ("тест:", "test"),
        ("quiz:", "test"),
        ("лекция:", "work"),
        ("lesson:", "work"),
        ("семинар:", "work"),
        ("workshop:", "work"),
        ("форум:", "work"),
        ("forum:", "work"),
        ("элемент курса:", "work"),
        ("ручная оценка:", "work"),
    ];
    let (p, k, kind) = kinds.iter().find_map(|(k, kind)| low.find(k).map(|p| (p, k.len(), *kind)))?;
    let item = h[p + k..].trim().to_string();
    // тип значения — в последних скобках
    let (name, typ) = match item.rfind(" (") {
        Some(i) if item.ends_with(')') => {
            let t = item[i + 2..item.len() - 1].to_lowercase();
            let typ = if t.starts_with("процент") || t.starts_with("percent") {
                "percent"
            } else if t.starts_with("букв") || t.starts_with("letter") {
                "letter"
            } else if t.starts_with("отзыв") || t.starts_with("feedback") || t.starts_with("комментар") {
                "feedback"
            } else if t.starts_with("значен") || t.starts_with("real") || t.starts_with("value") || t.starts_with("балл") {
                "value"
            } else {
                ""
            };
            if typ.is_empty() {
                (item.clone(), "value")
            } else {
                (item[..i].trim().to_string(), typ)
            }
        }
        _ => (item.clone(), "value"),
    };
    let code = super::text::find_work_code(&name).unwrap_or_else(|| name.clone());
    let kind = if kind == "work" && super::journal::auto_kind(&name, "") == "test" { "test" } else { kind };
    Some((code, name, kind, typ))
}

/// Код работы из заголовка столбца журнала оценок Moodle:
/// «Задание: Практическая работа 2 (Значение)» → «ПР2».
/// Уточнить шифр по номеру с подпунктом в названии: («ПР4», «Практическая работа 4.2. …») → «ПР4.2».
/// Если подпункта нет — полное название.
pub fn sub_code(code: &str, name: &str) -> String {
    let num: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if !num.is_empty() {
        let ch: Vec<char> = name.chars().collect();
        let nd: Vec<char> = num.chars().collect();
        let mut i = 0;
        while i + nd.len() < ch.len() {
            let before_ok = i == 0 || !ch[i - 1].is_ascii_digit();
            if before_ok && ch[i..i + nd.len()] == nd[..] && ch[i + nd.len()] == '.' {
                let mut j = i + nd.len() + 1;
                let st = j;
                while j < ch.len() && ch[j].is_ascii_digit() {
                    j += 1;
                }
                if j > st && j - st <= 2 {
                    let sub: String = ch[st..j].iter().collect();
                    return format!("{}.{}", code, sub);
                }
            }
            i += 1;
        }
    }
    name.to_string()
}

pub fn column_code(h: &str) -> Option<String> {
    column_info(h).map(|x| x.0)
}

/// Буква Moodle → пятибалльная оценка: A/B/C/D/E/F (с плюсами и минусами) или слова.
pub fn letter_to_five(s: &str) -> Option<f64> {
    let t = s.trim().to_lowercase();
    if t.is_empty() || t == "-" {
        return None;
    }
    if let Some(v) = grade_value(&t).filter(|v| (2.0..=5.0).contains(v) && t.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.')) {
        return Some(v);
    }
    let words: [(&str, f64); 8] = [("неуд", 2.0), ("не зач", 2.0), ("отлич", 5.0), ("хорош", 4.0), ("удовл", 3.0), ("зачт", 5.0), ("плох", 2.0), ("excellent", 5.0)];
    if let Some((_, v)) = words.iter().find(|(w, _)| t.starts_with(w)) {
        return Some(*v);
    }
    match t.chars().next()? {
        'a' | 'а' => Some(5.0),
        'b' | 'в' => Some(4.0),
        'c' | 'с' => Some(3.0),
        'd' | 'e' => Some(3.0),
        'f' => Some(2.0),
        _ => None,
    }
}

/// Шкала столбца «Значение» по самому большому числу: до 5 — пятибалльная, до 10, до 20 или 100.
pub fn guess_scale(vals: &[f64]) -> f64 {
    let max = vals.iter().cloned().fold(0.0, f64::max);
    if max <= 5.5 {
        5.0
    } else if max <= 10.0 {
        10.0
    } else if max <= 20.0 {
        20.0
    } else {
        100.0
    }
}

/// Отзыв из Moodle хранится в HTML: «<p>Хорошо</p>» → «Хорошо».
pub fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => {
                tag = false;
                out.push(' ');
            }
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Одна оценка из файла Moodle.
#[derive(Clone, Debug)]
pub struct Imported {
    pub student: String,
    pub email: String,
    pub code: String,
    pub grade: f64,
    pub comment: String,
    /// work | test
    pub kind: String,
    /// полное название элемента курса
    pub title: String,
}

/// Разбор таблицы Moodle: «таблица оценок» задания (одна работа, нужен `code`)
/// или экспорт журнала оценок курса (столбцы «Задание: …»).
pub fn import_grades(rows: &[Vec<String>], code: &str) -> Result<Vec<Imported>, String> {
    import_grades_as(rows, code, "auto")
}

/// `prefer`: какой столбец брать, если Moodle выгрузил оценку в нескольких видах —
/// auto (проценты → значение → буква), value, percent, letter.
/// Все студенты файла Moodle (ФИО, почта) — и те, у кого ещё нет ни одной оценки.
pub fn students_of(rows: &[Vec<String>]) -> Vec<(String, String)> {
    let Some(head) = rows.first() else { return Vec::new() };
    let c_email = col(head, &["адрес электронной почты", "email address", "электронная почта", "email"]);
    let c_full = col(head, &["полное имя", "full name", "фамилия и имя"]);
    let c_first = col(head, &["имя", "first name"]);
    let c_last = col(head, &["фамилия", "last name", "surname"]);
    if c_full.is_none() && c_first.is_none() && c_last.is_none() {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for r in rows.iter().skip(1) {
        let get = |c: Option<usize>| c.and_then(|i| r.get(i)).map(|x| x.trim().to_string()).unwrap_or_default();
        let full = get(c_full);
        let name = if full.is_empty() { format!("{} {}", get(c_last), get(c_first)).trim().to_string() } else { full };
        if !name.is_empty() && !out.iter().any(|(n, _)| same_person(n, &name)) {
            out.push((name, get(c_email)));
        }
    }
    out
}

pub fn import_grades_as(rows: &[Vec<String>], code: &str, prefer: &str) -> Result<Vec<Imported>, String> {
    let head = rows.first().ok_or("Файл пустой")?;
    let c_email = col(head, &["адрес электронной почты", "email address", "электронная почта", "email"]);
    let c_full = col(head, &["полное имя", "full name", "фамилия и имя"]);
    let c_first = col(head, &["имя", "first name"]);
    let c_last = col(head, &["фамилия", "last name", "surname"]);
    let name_of = |r: &Vec<String>| -> String {
        let get = |c: Option<usize>| c.and_then(|i| r.get(i)).map(|x| x.trim().to_string()).unwrap_or_default();
        let full = get(c_full);
        if !full.is_empty() {
            return full;
        }
        format!("{} {}", get(c_last), get(c_first)).trim().to_string()
    };
    let mut out = Vec::new();
    // таблица оценок одного задания
    if let (Some(c_grade), true) = (col(head, &["оценка", "grade"]), c_full.is_some()) {
        if head.iter().all(|h| column_code(h).is_none()) {
            if code.trim().is_empty() {
                return Err("Это таблица оценок одного задания — выберите, какой работе (ПР) она соответствует".into());
            }
            let c_max = col(head, &["максимальная оценка", "maximum grade"]);
            let c_fb = col(head, &["отзыв в виде комментария", "feedback comments"]);
            for r in rows.iter().skip(1) {
                let Some(v) = r.get(c_grade).and_then(|x| grade_value(x)) else { continue };
                let max = c_max.and_then(|i| r.get(i)).and_then(|x| grade_value(x)).unwrap_or(if v > 5.5 { 100.0 } else { 5.0 });
                let name = name_of(r);
                if name.is_empty() {
                    continue;
                }
                out.push(Imported {
                    student: name,
                    email: c_email.and_then(|i| r.get(i)).cloned().unwrap_or_default(),
                    code: code.trim().to_string(),
                    grade: to_five(v, max),
                    comment: c_fb.and_then(|i| r.get(i)).map(|x| strip_tags(x).chars().take(600).collect()).unwrap_or_default(),
                    kind: super::journal::auto_kind(code, "").to_string(),
                    title: String::new(),
                });
            }
            return Ok(out);
        }
    }
    // журнал оценок курса: по столбцу на каждый элемент и вид оценки
    let mut infos: Vec<(usize, (String, String, &str, &str))> = head.iter().enumerate().filter_map(|(i, h)| column_info(h).map(|c| (i, c))).collect();
    // «Практическая работа 4.1» и «4.2» дают один шифр ПР4 — разводим их, иначе оценки смешаются
    let mut names_of: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (_, (c, n, ..)) in &infos {
        let v = names_of.entry(c.clone()).or_default();
        if !v.contains(n) {
            v.push(n.clone());
        }
    }
    for (_, (c, n, ..)) in infos.iter_mut() {
        if names_of.get(c.as_str()).map(|v| v.len() > 1).unwrap_or(false) {
            *c = sub_code(c, n);
        }
    }
    let mut seen: std::collections::BTreeMap<String, String> = Default::default();
    for (_, (c, n, ..)) in infos.iter_mut() {
        match seen.get(c.as_str()) {
            Some(other) if other != n => *c = n.clone(),
            Some(_) => {}
            None => {
                seen.insert(c.clone(), n.clone());
            }
        }
    }
    if infos.is_empty() {
        return Err("Не понял формат файла. Подойдут: «таблица оценок» задания (Просмотр всех ответов → Действия с оценками → Скачать таблицу оценок) или экспорт журнала оценок курса (Оценки → Экспорт → CSV или Excel)".into());
    }
    if c_full.is_none() && c_last.is_none() && c_first.is_none() {
        return Err("В файле нет столбцов с именем и фамилией".into());
    }
    // коды по порядку появления
    let mut codes: Vec<String> = Vec::new();
    for (_, (c, ..)) in &infos {
        if !codes.contains(c) {
            codes.push(c.clone());
        }
    }
    let order: Vec<&str> = match prefer {
        "value" => vec!["value", "percent", "letter"],
        "percent" => vec!["percent", "value", "letter"],
        "letter" => vec!["letter", "percent", "value"],
        _ => vec!["percent", "value", "letter"],
    };
    for c in &codes {
        let cols: Vec<&(usize, (String, String, &str, &str))> = infos.iter().filter(|(_, x)| &x.0 == c).collect();
        let (title, kind) = cols.first().map(|(_, x)| (x.1.clone(), x.2)).unwrap_or_default();
        let fb = cols.iter().find(|(_, x)| x.3 == "feedback").map(|(i, _)| *i);
        let mut by_type: Vec<(usize, &str)> = Vec::new();
        for t in &order {
            if let Some((i, _)) = cols.iter().find(|(_, x)| x.3 == *t) {
                by_type.push((*i, t));
            }
        }
        // шкала столбца «Значение»
        let scale = |i: usize| guess_scale(&rows.iter().skip(1).filter_map(|r| r.get(i).and_then(|x| grade_value(x))).collect::<Vec<_>>());
        let scales: Vec<f64> = by_type.iter().map(|(i, t)| if *t == "value" { scale(*i) } else { 100.0 }).collect();
        for r in rows.iter().skip(1) {
            let name = name_of(r);
            if name.is_empty() {
                continue;
            }
            // первая из колонок в порядке предпочтения, где есть оценка
            let g = by_type.iter().zip(&scales).find_map(|((i, t), sc)| {
                let cell = r.get(*i)?;
                match *t {
                    "letter" => letter_to_five(cell),
                    "percent" => grade_value(cell).map(|v| to_five(v, 100.0)),
                    _ => grade_value(cell).map(|v| to_five(v, *sc)),
                }
            });
            let Some(g) = g else { continue };
            out.push(Imported {
                student: name,
                email: c_email.and_then(|k| r.get(k)).cloned().unwrap_or_default(),
                code: c.clone(),
                grade: g,
                comment: fb.and_then(|k| r.get(k)).map(|x| strip_tags(x).chars().take(600).collect()).unwrap_or_default(),
                kind: kind.to_string(),
                title: title.clone(),
            });
        }
    }
    Ok(out)
}

/// CSV для «Оценки → Импорт → Файл CSV» в Moodle: почта, ФИО и по столбцу на работу.
pub fn export_gradebook(students: &[(String, String)], codes: &[String], grade: &dyn Fn(&str, &str) -> Option<f64>) -> String {
    export_gradebook_full(students, codes, grade, None, ';')
}

/// То же, с отзывами (столбец «<работа> (отзыв)» сразу после оценки) и выбором разделителя.
pub fn export_gradebook_full(students: &[(String, String)], codes: &[String], grade: &dyn Fn(&str, &str) -> Option<f64>, feedback: Option<&dyn Fn(&str, &str) -> String>, sep: char) -> String {
    let mut head = vec!["Адрес электронной почты".to_string(), "Полное имя".to_string()];
    for c in codes {
        head.push(c.clone());
        if feedback.is_some() {
            head.push(format!("{} (отзыв)", c));
        }
    }
    let mut rows: Vec<Vec<String>> = vec![head];
    for (name, email) in students {
        let mut r = vec![email.clone(), name.clone()];
        let mut any = false;
        for c in codes {
            match grade(name, c) {
                Some(g) => {
                    any = true;
                    r.push(super::core::fmt_grade(g).replace(',', "."));
                }
                None => r.push(String::new()),
            }
            if let Some(f) = feedback {
                r.push(f(name, c));
            }
        }
        if any {
            rows.push(r);
        }
    }
    write_csv(&rows, sep).trim_start_matches('\u{feff}').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradebook_subcodes_and_decimals() {
        assert_eq!(sub_code("ПР4", "Практическая работа 4.2. Табулирование"), "ПР4.2");
        assert_eq!(sub_code("ПР1", "Практическая работа №1. Аппаратное"), "Практическая работа №1. Аппаратное");
        let info = column_info("Итоговая оценка за курс (Значение)").unwrap();
        assert_eq!((info.0.as_str(), info.2, info.3), ("Итог курса", "custom", "value"));
        let row = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
        let rows = vec![
            row(&["Имя", "Фамилия", "Задание:Практическая работа 4.1. Основы (Значение)", "Задание:Практическая работа 4.2. Графики (Значение)", "Тест:Тестирование по Разделу 1 (Значение)", "Тест:Тестирование по Разделу 1 (Отзыв)", "Итоговая оценка за курс (Значение)"]),
            row(&["Тихон", "Громобоев", "3", "4,5", "2,6", "<p>без ИИ не обошлось</p>", "3,24"]),
        ];
        let got = import_grades_as(&rows, "", "auto").unwrap();
        let g = |c: &str| got.iter().find(|x| x.code == c).map(|x| x.grade);
        assert_eq!(g("ПР4.1"), Some(3.0));
        assert_eq!(g("ПР4.2"), Some(4.5));
        assert!((g("Тестирование по Разделу 1").unwrap() - 2.6).abs() < 1e-9);
        assert_eq!(got.iter().find(|x| x.code == "Итог курса").map(|x| x.kind.as_str()), Some("custom"));
        assert!(got.iter().any(|x| x.comment.contains("ИИ")));
        assert!(super::super::journal::mentions_no_variant("Вариант?"));
        assert!(super::super::journal::mentions_no_variant("какой  вариант? Файл не открывается"));
        assert!(!super::super::journal::mentions_no_variant("Вариант 3, всё верно"));
        assert_eq!(super::super::journal::comment_flags("ИИ. Вариант?"), "ai_text,no_variant");
    }

    #[test]
    fn names() {
        assert_eq!(parse_name("Лесовиков Иван_123456_assignsubmission_file_ПР2.docx"), Some(("Лесовиков Иван".into(), "123456".into())));
        assert_eq!(parse_name("Громобоева Анна Сергеевна_77_assignsubmission_file"), Some(("Громобоева Анна Сергеевна".into(), "77".into())));
        assert_eq!(parse_name("Ли_Ким_5_assignsubmission_onlinetext_onlinetext.html"), Some(("Ли_Ким".into(), "5".into())));
        assert_eq!(parse_name("просто файл.docx"), None);
    }

    #[test]
    fn csv_roundtrip() {
        let t = "\u{feff}Идентификатор,Полное имя,Оценка\r\n\"Участник 1\",\"Лесовиков, Иван\",\r\nУчастник 2,Громобоев Пётр,\"4,5\"\r\n";
        let (rows, sep) = parse_csv(t);
        assert_eq!(sep, ',');
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1][1], "Лесовиков, Иван");
        assert_eq!(rows[2][2], "4,5");
        let back = write_csv(&rows, sep);
        assert_eq!(parse_csv(&back).0, rows);
        assert_eq!(name_key("Громобоев  Пётр"), name_key("Пётр Громобоев"));
        assert!(same_person("Лесовиков Иван", "Лесовиков Иван Иванович"));
        assert!(!same_person("Лесовиков Иван", "Лесовиков Пётр"));
    }

    #[test]
    fn import_formats() {
        // таблица оценок задания (100-балльная)
        let csv = "Идентификатор;Полное имя;Адрес электронной почты;Статус;Оценка;Максимальная оценка;Отзыв в виде комментария\nУчастник 1;Лесовиков Иван;ivan@example.ru;Отправлено;85,00;100,00;<p>Хорошо</p>\nУчастник 2;Громобоева Анна;anna@example.ru;Нет ответа;;100,00;";
        let rows = parse_csv(csv).0;
        assert!(import_grades(&rows, "").is_err());
        let g = import_grades(&rows, "ПР2").unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].student.as_str(), g[0].code.as_str(), g[0].email.as_str()), ("Лесовиков Иван", "ПР2", "ivan@example.ru"));
        assert!((g[0].grade - 4.25).abs() < 1e-9 && g[0].comment == "Хорошо");
        // журнал оценок курса
        let csv = "Имя,Фамилия,Адрес электронной почты,Задание: Практическая работа 1 (Значение),Задание: ПР2 Сети (Значение),Итоговая оценка за курс (Значение)\nАнна,Громобоева,anna@example.ru,5.00,-,4.50\nПётр,Ветерков,petr@example.ru,3.00,4.00,3.50";
        let g = import_grades(&parse_csv(csv).0, "").unwrap();
        let codes: Vec<&str> = g.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, vec!["ПР1", "ПР1", "ПР2", "Итог курса", "Итог курса"]);
        assert_eq!(g[0].student, "Громобоева Анна");
        // проценты, буквы, отзывы, тесты
        let csv = "Фамилия;Имя;Адрес электронной почты;Задание: ПР1 Сети (Значение);Задание: ПР1 Сети (Проценты);Задание: ПР1 Сети (Буква);Задание: ПР1 Сети (Отзыв);Тест: Тестирование по разделу 1 (Значение)\nГромобоева;Анна;anna@example.ru;8,00;80,00 %;B-;<p>Похоже на ответ нейросети</p>;7,00\nВетерков;Пётр;petr@example.ru;10,00;100,00 %;A;;3,00";
        let g = import_grades_as(&parse_csv(csv).0, "", "auto").unwrap();
        let pr = g.iter().find(|x| x.code == "ПР1" && x.student == "Громобоева Анна").unwrap();
        assert!((pr.grade - 4.0).abs() < 1e-9, "{}", pr.grade);
        assert_eq!(pr.comment, "Похоже на ответ нейросети");
        let t = g.iter().find(|x| x.kind == "test" && x.student == "Ветерков Пётр").unwrap();
        assert_eq!(t.code, "Тестирование по разделу 1");
        assert!((t.grade - 1.5).abs() < 1e-9, "{}", t.grade);
        let l = import_grades_as(&parse_csv(csv).0, "", "letter").unwrap();
        assert_eq!(l.iter().find(|x| x.code == "ПР1" && x.student == "Ветерков Пётр").unwrap().grade, 5.0);
        assert_eq!(letter_to_five("неудовлетворительно"), Some(2.0));
        assert_eq!(encode_csv("Оценка;5\r\n", "cp1251"), vec![0xCE, 0xF6, 0xE5, 0xED, 0xEA, 0xE0, b';', b'5', b'\r', b'\n']);
        assert!(encode_csv("x", "utf8bom").starts_with(&[0xEF, 0xBB, 0xBF]));
        let out = export_gradebook(&[("Громобоева Анна".into(), "anna@example.ru".into())], &["ПР1".into()], &|_, _| Some(4.5));
        assert!(out.starts_with("Адрес электронной почты;Полное имя;ПР1") && out.contains("anna@example.ru;Громобоева Анна;4.5"));
    }
}
