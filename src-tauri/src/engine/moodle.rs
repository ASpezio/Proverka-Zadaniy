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

/// Код работы из заголовка столбца журнала оценок Moodle:
/// «Задание: Практическая работа 2 (Значение)» → «ПР2».
pub fn column_code(h: &str) -> Option<String> {
    let low = h.to_lowercase();
    let item = ["задание:", "assignment:", "тест:", "quiz:"].iter().find_map(|k| low.find(k).map(|p| h[p + k.len()..].to_string()))?;
    let item = item.split(" (").next().unwrap_or(&item).trim().to_string();
    Some(super::text::find_work_code(&item).unwrap_or(item))
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
}

/// Разбор таблицы Moodle: «таблица оценок» задания (одна работа, нужен `code`)
/// или экспорт журнала оценок курса (столбцы «Задание: …»).
pub fn import_grades(rows: &[Vec<String>], code: &str) -> Result<Vec<Imported>, String> {
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
                    comment: c_fb.and_then(|i| r.get(i)).map(|x| strip_tags(x).chars().take(300).collect()).unwrap_or_default(),
                });
            }
            return Ok(out);
        }
    }
    // журнал оценок курса: по столбцу на каждое задание
    let cols: Vec<(usize, String)> = head.iter().enumerate().filter_map(|(i, h)| column_code(h).map(|c| (i, c))).collect();
    if cols.is_empty() {
        return Err("Не понял формат файла. Подойдут: «таблица оценок» задания (Просмотр всех ответов → Действия с оценками → Скачать таблицу оценок) или экспорт журнала оценок курса (Оценки → Экспорт → CSV или Excel)".into());
    }
    if c_full.is_none() && c_last.is_none() && c_first.is_none() {
        return Err("В файле нет столбцов с именем и фамилией".into());
    }
    // шкала: если где-то больше 5 — считаем стобалльной
    for (i, c) in &cols {
        let vals: Vec<f64> = rows.iter().skip(1).filter_map(|r| r.get(*i).and_then(|x| grade_value(x))).collect();
        let max = if vals.iter().any(|v| *v > 5.5) { 100.0 } else { 5.0 };
        for r in rows.iter().skip(1) {
            let Some(v) = r.get(*i).and_then(|x| grade_value(x)) else { continue };
            let name = name_of(r);
            if name.is_empty() {
                continue;
            }
            out.push(Imported { student: name, email: c_email.and_then(|k| r.get(k)).cloned().unwrap_or_default(), code: c.clone(), grade: to_five(v, max), comment: String::new() });
        }
    }
    Ok(out)
}

/// CSV для «Оценки → Импорт → Файл CSV» в Moodle: почта, ФИО и по столбцу на работу.
pub fn export_gradebook(students: &[(String, String)], codes: &[String], grade: &dyn Fn(&str, &str) -> Option<f64>) -> String {
    let mut rows: Vec<Vec<String>> = vec![std::iter::once("Адрес электронной почты".to_string()).chain(std::iter::once("Полное имя".to_string())).chain(codes.iter().cloned()).collect()];
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
        }
        if any {
            rows.push(r);
        }
    }
    write_csv(&rows, ';').trim_start_matches('\u{feff}').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(codes, vec!["ПР1", "ПР1", "ПР2"]);
        assert_eq!(g[0].student, "Громобоева Анна");
        let out = export_gradebook(&[("Громобоева Анна".into(), "anna@example.ru".into())], &["ПР1".into()], &|_, _| Some(4.5));
        assert!(out.starts_with("Адрес электронной почты;Полное имя;ПР1") && out.contains("anna@example.ru;Громобоева Анна;4.5"));
    }
}
