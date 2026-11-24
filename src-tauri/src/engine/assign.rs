//! Задания, ответы (эталоны) и их разбиение на варианты.
//! Здесь же вычисляются ловушки против ИИ для каждого варианта.

use super::docx::{hidden_of, visible_of, HID_END, HID_START};
use super::json::Json;
use super::text;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum DocKind {
    Task,
    Key,
    Example,
}

impl DocKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocKind::Task => "task",
            DocKind::Key => "key",
            DocKind::Example => "example",
        }
    }
    pub fn from_str(s: &str) -> DocKind {
        match s {
            "key" => DocKind::Key,
            "example" => DocKind::Example,
            _ => DocKind::Task,
        }
    }
    /// Угадываем по имени файла.
    /// По полному пути: всё из папки «Пример выполнения» — образец, памятки и методички — тоже.
    pub fn guess_path(path: &str) -> DocKind {
        let p = std::path::Path::new(path);
        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let parent = p.parent().and_then(|x| x.file_name()).map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let l = name.to_lowercase();
        if l.contains("ответ") || l.contains("эталон") || l.contains("ключ") {
            return DocKind::Key;
        }
        if parent.contains("пример") || parent.contains("образец") || l.contains("памятк") || l.contains("методич") {
            return DocKind::Example;
        }
        DocKind::guess(&name)
    }

    pub fn guess(name: &str) -> DocKind {
        let l = name.to_lowercase();
        if l.contains("ответ") || l.contains("эталон") || l.contains("ключ") || l.contains("answer") || l.contains("критери") {
            DocKind::Key
        } else if l.contains("пример") || l.contains("образец") || l.contains("example") {
            DocKind::Example
        } else {
            DocKind::Task
        }
    }
}

/// Загруженный файл задания/ответов.
#[derive(Clone, Debug)]
pub struct TaskDoc {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: DocKind,
    pub code: String, // ПР2 или имя файла
    /// для кого задание: группа или направление («СПО-11», «1гр»); пусто — для всех
    pub scope: String,
    pub marked: String,
    pub notes: Vec<String>,
    pub needs_pdf: bool,
}

/// Один вариант набора заданий.
#[derive(Clone, Debug, Default)]
pub struct Variant {
    pub id: String,
    pub task_visible: String,
    pub hidden: Vec<String>,
    pub key: String,
    pub ai_row: String,
    pub markers: Vec<String>,
}

/// Набор (например, «ПР2») — все варианты задания + ответы.
#[derive(Clone, Debug, Default)]
pub struct AssignmentSet {
    /// уникальный ключ набора: «ПР2» или «ПР2@СПО-9»
    pub id: String,
    pub code: String,
    pub scope: String,
    pub title: String,
    pub variants: BTreeMap<String, Variant>,
    pub general_key: String, // часть файла ответов до первого варианта (описание защиты и т.п.)
    pub example: String,
    pub files: Vec<String>,
    pub notes: Vec<String>,
}

impl AssignmentSet {
    pub fn variant(&self, id: &str) -> Option<&Variant> {
        self.variants.get(id).or_else(|| self.variants.get("*"))
    }
    pub fn hidden_total(&self) -> usize {
        self.variants.values().map(|v| v.hidden.len()).sum()
    }
    pub fn has_key(&self) -> bool {
        self.variants.values().any(|v| !v.key.is_empty()) || !self.general_key.is_empty()
    }
    pub fn to_json(&self, full: bool) -> Json {
        let mut vars = Json::arr();
        for v in self.variants.values() {
            let mut o = Json::obj()
                .with("id", v.id.as_str())
                .with("hidden_count", v.hidden.len())
                .with("has_key", !v.key.is_empty())
                .with("markers", v.markers.clone());
            if full {
                o = o
                    .with("task", v.task_visible.as_str())
                    .with("hidden", v.hidden.clone())
                    .with("key", v.key.as_str())
                    .with("ai_row", v.ai_row.as_str());
            }
            vars.push(o);
        }
        Json::obj()
            .with("id", self.id.as_str())
            .with("code", self.code.as_str())
            .with("scope", self.scope.as_str())
            .with("title", self.title.as_str())
            .with("variants", vars)
            .with("variant_count", self.variants.len())
            .with("hidden_total", self.hidden_total())
            .with("has_key", self.has_key())
            .with("has_example", !self.example.is_empty())
            .with("files", self.files.clone())
            .with("notes", self.notes.clone())
    }
}

/// Ключ набора с учётом того, для кого он.
pub fn set_key(code: &str, scope: &str) -> String {
    if scope.trim().is_empty() {
        code.to_string()
    } else {
        format!("{}@{}", code, scope.trim())
    }
}

/// Строка — заголовок варианта? Возвращает номер.
fn variant_heading(line: &str) -> Option<String> {
    let clean: String = line.chars().filter(|c| *c != HID_START && *c != HID_END).collect();
    let t = clean.trim().trim_end_matches(|c: char| c == '.' || c == ':');
    let l = t.to_lowercase();
    if t.chars().count() > 16 {
        return None;
    }
    let rest = l.strip_prefix("вариант")?;
    let rest = rest.trim().trim_start_matches('№').trim();
    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) && rest.len() <= 3 {
        let r = rest.trim_start_matches('0');
        return Some(if r.is_empty() { "0".into() } else { r.to_string() });
    }
    None
}

/// Делит текст (с маркерами) на преамбулу и варианты.
pub fn split_variants(marked: &str) -> (String, Vec<(String, String)>) {
    let mut pre = String::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    for line in marked.lines() {
        if let Some(v) = variant_heading(line) {
            parts.push((v, String::new()));
            continue;
        }
        match parts.last_mut() {
            Some((_, body)) => {
                body.push_str(line);
                body.push('\n');
            }
            None => {
                pre.push_str(line);
                pre.push('\n');
            }
        }
    }
    (pre, parts)
}

/// Слова/фразы в «ёлочках» — кандидаты в маркеры.
fn quoted_terms(s: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find('«') {
        let after = &rest[a + '«'.len_utf8()..];
        match after.find('»') {
            Some(b) => {
                let t = after[..b].trim();
                if t.chars().count() >= 4 && t.chars().count() <= 60 {
                    res.push(t.to_string());
                }
                rest = &after[b + '»'.len_utf8()..];
            }
            None => break,
        }
    }
    res
}

/// Строка «ИИ» из таблицы ответов (признаки использования нейросети).
fn ai_row_of(key_text: &str) -> String {
    for line in key_text.lines() {
        let t = line.trim();
        let l = t.to_lowercase();
        if l.starts_with("ии |") || l.starts_with("ии|") || l.starts_with("признаки ии") || l.contains("признаки ии:") {
            return t.trim_start_matches("ИИ").trim_start_matches('|').trim().to_string();
        }
    }
    String::new()
}

/// Собирает наборы заданий из загруженных файлов.
pub fn build_sets(docs: &[TaskDoc], custom_markers: &[String]) -> Vec<AssignmentSet> {
    let mut by_code: BTreeMap<String, AssignmentSet> = BTreeMap::new();
    // если в наборе один код у всех — всё в один набор
    for d in docs {
        let key = set_key(&d.code, &d.scope);
        let set = by_code.entry(key.clone()).or_insert_with(|| AssignmentSet { id: key.clone(), code: d.code.clone(), scope: d.scope.clone(), ..Default::default() });
        set.files.push(d.name.clone());
        for n in &d.notes {
            set.notes.push(format!("{}: {}", d.name, n));
        }
        if d.needs_pdf && d.marked.trim().is_empty() {
            set.notes.push(format!("{}: ожидается извлечение текста из PDF", d.name));
            continue;
        }
        let (pre, parts) = split_variants(&d.marked);
        match d.kind {
            DocKind::Task => {
                if set.title.is_empty() {
                    set.title = title_of(&pre, &parts, &d.name);
                }
                if parts.is_empty() {
                    // файл без деления на варианты (или файл одного варианта: «Задание_В0»)
                    let vid = text::find_variant(&d.name).unwrap_or_else(|| "*".into());
                    let v = set.variants.entry(vid.clone()).or_insert_with(|| Variant { id: vid.clone(), ..Default::default() });
                    v.task_visible = visible_of(&d.marked);
                    v.hidden = hidden_of(&d.marked);
                } else {
                    for (vid, body) in parts {
                        let v = set.variants.entry(vid.clone()).or_insert_with(|| Variant { id: vid.clone(), ..Default::default() });
                        let full = format!("{}\n{}", pre_title_lines(&pre), body);
                        v.task_visible = visible_of(&full);
                        v.hidden = hidden_of(&body);
                    }
                }
            }
            DocKind::Key => {
                if parts.is_empty() {
                    let vid = text::find_variant(&d.name);
                    match vid {
                        Some(vid) => {
                            let v = set.variants.entry(vid.clone()).or_insert_with(|| Variant { id: vid.clone(), ..Default::default() });
                            v.key = visible_of(&d.marked);
                            v.ai_row = ai_row_of(&v.key);
                        }
                        None => set.general_key = visible_of(&d.marked),
                    }
                } else {
                    set.general_key = visible_of(&pre);
                    for (vid, body) in parts {
                        let v = set.variants.entry(vid.clone()).or_insert_with(|| Variant { id: vid.clone(), ..Default::default() });
                        v.key = visible_of(&body);
                        v.ai_row = ai_row_of(&v.key);
                    }
                }
            }
            DocKind::Example => {
                if !set.example.is_empty() {
                    set.example.push_str("\n\n");
                }
                set.example.push_str(&text::clip(&visible_of(&d.marked), 6000));
            }
        }
    }
    // маркеры для каждого варианта
    for set in by_code.values_mut() {
        let all_visible: String = set.variants.values().map(|v| v.task_visible.as_str()).collect::<Vec<_>>().join("\n");
        let vis_norm = text::norm(&all_visible);
        for v in set.variants.values_mut() {
            let mut m: Vec<String> = Vec::new();
            let mut cands: Vec<String> = Vec::new();
            for h in &v.hidden {
                cands.extend(quoted_terms(h));
            }
            cands.extend(quoted_terms(&v.ai_row));
            for c in cands {
                if !text::contains_phrase(&vis_norm, &c) && !m.iter().any(|x| x.eq_ignore_ascii_case(&c)) {
                    m.push(c);
                }
            }
            for c in custom_markers {
                if !c.trim().is_empty() && !m.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                    m.push(c.trim().to_string());
                }
            }
            v.markers = m;
        }
        if set.title.is_empty() {
            set.title = set.code.clone();
        }
        if set.variants.is_empty() {
            set.notes.push("Не найдено ни одного варианта задания".into());
        }
    }
    by_code.into_values().collect()
}

/// Первые строки преамбулы задания (название работы), без инструкций.
fn pre_title_lines(pre: &str) -> String {
    pre.lines().take(4).collect::<Vec<_>>().join("\n")
}

fn title_of(pre: &str, parts: &[(String, String)], name: &str) -> String {
    let src = if pre.trim().is_empty() { parts.first().map(|p| p.1.as_str()).unwrap_or("") } else { pre };
    let vis = visible_of(src);
    let lines: Vec<&str> = vis.lines().filter(|l| !l.trim().is_empty()).take(2).collect();
    if lines.is_empty() {
        return name.to_string();
    }
    lines.join(". ").chars().take(140).collect()
}
