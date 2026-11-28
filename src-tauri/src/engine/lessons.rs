//! Чему приложение учится у преподавателя:
//! * **уроки** (`lessons.json`) — работы, где преподаватель не согласился с оценкой
//!   нейросети: что поставила модель, что преподаватель и почему. В следующих запросах
//!   к тому же заданию модель получает 2–3 самых похожих примера и оценивает в той же
//!   манере. Текст работы не хранится — только «отпечаток» из основ слов для поиска
//!   похожих, короткий итог модели и комментарий преподавателя.
//! * **банк замечаний** (`remarks.json`) — фразы, которые преподаватель пишет в отзывах
//!   и комментариях: частые предлагаются одним щелчком.

use super::json::{self, Json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Один файл пишут несколько потоков — по очереди.
static LOCK: Mutex<()> = Mutex::new(());

// ------------------------------------------------------------------ уроки

#[derive(Clone, Debug, Default)]
pub struct Lesson {
    /// работа (повторная правка той же работы заменяет прежнюю)
    pub key: String,
    /// шифр работы (ПР3) — примеры берутся только для того же задания
    pub code: String,
    pub variant: String,
    pub model_grade: f64,
    pub teacher_grade: f64,
    /// почему (комментарий преподавателя), может быть пустым
    pub why: String,
    /// что написала модель: итог и главные ошибки, коротко
    pub model_said: String,
    /// отпечаток содержания: основы слов (хэши)
    pub sketch: Vec<u32>,
    pub date: u64,
}

fn lessons_path(dir: &Path) -> PathBuf {
    dir.join("lessons.json")
}

/// Отпечаток содержания для поиска похожих работ: до 300 самых частых основ слов
/// (первые 6 букв слов от 4 букв). Восстановить по нему текст нельзя.
pub fn sketch(text: &str) -> Vec<u32> {
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for w in super::text::words(text) {
        let ch: Vec<char> = w.chars().collect();
        if ch.len() < 4 || ch.iter().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let stem: String = ch.iter().take(6).collect();
        let mut h: u32 = 2166136261;
        for b in stem.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(16777619);
        }
        *freq.entry(h).or_default() += 1;
    }
    let mut v: Vec<(u32, usize)> = freq.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out: Vec<u32> = v.into_iter().take(300).map(|x| x.0).collect();
    out.sort_unstable();
    out
}

fn similarity(a: &[u32], b: &[u32]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa: HashSet<&u32> = a.iter().collect();
    let common = b.iter().filter(|x| sa.contains(x)).count();
    common as f64 / (a.len().min(b.len())) as f64
}

pub fn lessons_load(dir: &Path) -> Vec<Lesson> {
    let Ok(t) = std::fs::read_to_string(lessons_path(dir)) else { return Vec::new() };
    let Ok(j) = json::parse(&t) else { return Vec::new() };
    j.as_arr()
        .iter()
        .map(|r| Lesson {
            key: r.get("key").str_or(""),
            code: r.get("code").str_or(""),
            variant: r.get("variant").str_or(""),
            model_grade: r.get("model").f64_or(0.0),
            teacher_grade: r.get("teacher").f64_or(0.0),
            why: r.get("why").str_or(""),
            model_said: r.get("model_said").str_or(""),
            sketch: r.get("sketch").as_arr().iter().filter_map(|x| x.as_f64()).map(|x| x as u32).collect(),
            date: r.get("date").f64_or(0.0) as u64,
        })
        .collect()
}

fn lessons_save(dir: &Path, all: &[Lesson]) {
    let arr: Vec<Json> = all
        .iter()
        .map(|r| {
            Json::obj()
                .with("key", r.key.as_str())
                .with("code", r.code.as_str())
                .with("variant", r.variant.as_str())
                .with("model", r.model_grade)
                .with("teacher", r.teacher_grade)
                .with("why", r.why.as_str())
                .with("model_said", r.model_said.as_str())
                .with("sketch", Json::Arr(r.sketch.iter().map(|x| Json::Num(*x as f64)).collect()))
                .with("date", r.date)
        })
        .collect();
    let _ = std::fs::write(lessons_path(dir), Json::Arr(arr).to_string());
}

/// Запомнить правку (та же работа — заменить). Хранится не больше 2000 последних.
pub fn lesson_put(dir: &Path, l: Lesson) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = lessons_load(dir);
    all.retain(|x| x.key != l.key);
    all.push(l);
    if all.len() > 2000 {
        let cut = all.len() - 2000;
        all.drain(..cut);
    }
    lessons_save(dir, &all);
}

/// Преподаватель вернул оценку модели — урок больше не нужен.
pub fn lesson_forget(dir: &Path, key: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = lessons_load(dir);
    let n = all.len();
    all.retain(|x| x.key != key);
    if all.len() != n {
        lessons_save(dir, &all);
    }
}

pub fn lessons_clear(dir: &Path, code: &str) -> usize {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = lessons_load(dir);
    let n = all.len();
    all.retain(|x| !code.is_empty() && x.code != code);
    lessons_save(dir, &all);
    n - all.len()
}

/// До `n` примеров для работы: то же задание; сначала похожие по содержанию и того же
/// варианта, затем свежие. Сама работа (`key`) в примеры не попадает.
pub fn lessons_for(dir: &Path, code: &str, variant: &str, key: &str, text: &str, n: usize) -> Vec<Lesson> {
    if code.is_empty() || n == 0 {
        return Vec::new();
    }
    let all: Vec<Lesson> = lessons_load(dir).into_iter().filter(|l| l.code == code && l.key != key).collect();
    if all.is_empty() {
        return Vec::new();
    }
    let me = sketch(text);
    let mut scored: Vec<(f64, Lesson)> = all
        .into_iter()
        .map(|l| {
            let s = similarity(&me, &l.sketch) + if !variant.is_empty() && l.variant == variant { 0.15 } else { 0.0 } + (l.date as f64) * 1e-12;
            (s, l)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(n).map(|x| x.1).collect()
}

/// Раздел запроса с примерами (пусто — примеров нет).
pub fn lessons_prompt(list: &[Lesson], fmt: impl Fn(f64) -> String) -> String {
    if list.is_empty() {
        return String::new();
    }
    let mut s = String::from("# КАК ЭТОТ ПРЕПОДАВАТЕЛЬ ИСПРАВЛЯЛ ОЦЕНКИ ЗА ЭТО ЗАДАНИЕ\nПохожие работы других студентов, где преподаватель не согласился с оценкой нейросети. Оценивай в его манере: так же строго или мягко и за то же самое. Это примеры, а не эта работа — оценку ставь по её содержанию.\n");
    for (i, l) in list.iter().enumerate() {
        let dir = if l.teacher_grade < l.model_grade { "ниже" } else { "выше" };
        s.push_str(&format!("{}. Нейросеть поставила {}, преподаватель — {} ({}).", i + 1, fmt(l.model_grade), fmt(l.teacher_grade), dir));
        if l.why.trim().is_empty() {
            s.push_str(" Причину не указал.");
        } else {
            s.push_str(&format!(" Причина: «{}».", super::text::clip(l.why.trim(), 400)));
        }
        if !l.model_said.trim().is_empty() {
            s.push_str(&format!(" Нейросеть тогда написала: «{}».", super::text::clip(l.model_said.trim(), 400)));
        }
        s.push('\n');
    }
    s.push('\n');
    s
}

/// Сводка для окна «Чему научилась проверка»: по заданиям — сколько правок и в какую сторону.
pub fn lessons_summary(dir: &Path) -> Json {
    let all = lessons_load(dir);
    let mut by: HashMap<String, (usize, usize, usize)> = HashMap::new();
    for l in &all {
        let e = by.entry(l.code.clone()).or_default();
        e.0 += 1;
        if l.teacher_grade < l.model_grade {
            e.1 += 1;
        } else {
            e.2 += 1;
        }
    }
    let mut codes: Vec<(String, (usize, usize, usize))> = by.into_iter().collect();
    codes.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    let list: Vec<Json> = codes.into_iter().map(|(c, (n, down, up))| Json::obj().with("code", c).with("count", n).with("lower", down).with("higher", up)).collect();
    Json::obj().with("total", all.len()).with("codes", Json::Arr(list))
}

// ------------------------------------------------------------------ банк замечаний

#[derive(Clone, Debug, Default)]
pub struct Remark {
    pub text: String,
    /// сколько раз встречалась в правках
    pub count: usize,
    /// добавлена вручную — всегда наверху, не вытесняется
    pub pinned: bool,
    /// задания, где встречалась (для подсказок по текущему заданию)
    pub codes: Vec<String>,
    pub last: u64,
}

fn remarks_path(dir: &Path) -> PathBuf {
    dir.join("remarks.json")
}

pub fn remarks_load(dir: &Path) -> Vec<Remark> {
    let Ok(t) = std::fs::read_to_string(remarks_path(dir)) else { return Vec::new() };
    let Ok(j) = json::parse(&t) else { return Vec::new() };
    j.as_arr()
        .iter()
        .map(|r| Remark {
            text: r.get("text").str_or(""),
            count: r.get("count").i64_or(1).max(0) as usize,
            pinned: r.get("pinned").bool_or(false),
            codes: r.get("codes").str_list(),
            last: r.get("last").f64_or(0.0) as u64,
        })
        .filter(|r| !r.text.trim().is_empty())
        .collect()
}

fn remarks_save(dir: &Path, all: &[Remark]) {
    let arr: Vec<Json> = all
        .iter()
        .map(|r| Json::obj().with("text", r.text.as_str()).with("count", r.count).with("pinned", r.pinned).with("codes", r.codes.clone()).with("last", r.last))
        .collect();
    let _ = std::fs::write(remarks_path(dir), Json::Arr(arr).to_string());
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace('ё', "е").chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Фразы-замечания из текста: строки и предложения длиной 8–160 знаков, без ФИО
/// студента и без служебных строк отзыва (оценка, приветствие).
pub fn phrases(text: &str, student: &str) -> Vec<String> {
    let names: Vec<String> = student.split_whitespace().filter(|x| x.chars().count() >= 3).map(norm).collect();
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(|c: char| c == '-' || c == '•' || c == '*' || c == '—' || c.is_ascii_digit() || c == '.' || c == ')' || c == ' ');
        // на предложения: «. », «; », «! », «? »
        let mut cur = String::new();
        let chars: Vec<char> = line.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            cur.push(*c);
            let end = matches!(c, '.' | ';' | '!' | '?') && chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(true);
            if end {
                out.push(cur.trim().to_string());
                cur.clear();
            }
        }
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
    }
    out.into_iter()
        .map(|p| p.trim_end_matches([';']).trim().to_string())
        .filter(|p| {
            let n = p.chars().count();
            let low = norm(p);
            let words = low.split_whitespace().count();
            (8..=160).contains(&n)
                && words >= 2
                && low.chars().any(|c| c.is_alphabetic())
                && !names.iter().any(|nm| low.split_whitespace().any(|w| w == nm))
                && !is_address(p)
                && !["оценка", "здравствуйте", "добрый день", "итог", "работа студента", "вариант"].iter().any(|s| low.starts_with(s))
        })
        .collect()
}

/// Обращение к студенту («Иван, работа хорошая»): имя с заглавной и сразу запятая.
fn is_address(p: &str) -> bool {
    let first: String = p.chars().take_while(|c| c.is_alphabetic() || *c == '-').collect();
    first.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) && first.chars().count() >= 2 && p[first.len()..].starts_with(',')
}

/// Учесть замечания из правки преподавателя (`before` — что было до правки:
/// фразы, которые он не трогал, не считаются его замечаниями).
pub fn remarks_learn(dir: &Path, code: &str, text: &str, before: &str, student: &str) {
    let old: HashSet<String> = phrases(before, student).iter().map(|p| norm(p)).collect();
    let new: Vec<String> = phrases(text, student).into_iter().filter(|p| !old.contains(&norm(p))).collect();
    if new.is_empty() {
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = remarks_load(dir);
    let now = super::core::now_s();
    for p in new {
        let k = norm(&p);
        if let Some(r) = all.iter_mut().find(|r| norm(&r.text) == k) {
            r.count += 1;
            r.last = now;
            if !code.is_empty() && !r.codes.contains(&code.to_string()) {
                r.codes.push(code.to_string());
            }
        } else {
            all.push(Remark { text: p, count: 1, pinned: false, codes: if code.is_empty() { vec![] } else { vec![code.to_string()] }, last: now });
        }
    }
    // не больше 400: вытесняются редкие и давние, закреплённые — никогда
    if all.len() > 400 {
        all.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.count.cmp(&a.count)).then(b.last.cmp(&a.last)));
        all.truncate(400);
    }
    remarks_save(dir, &all);
}

pub fn remark_add(dir: &Path, text: &str) -> Result<(), String> {
    let t = text.trim();
    if t.chars().count() < 2 {
        return Err("Пустое замечание".into());
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = remarks_load(dir);
    if let Some(r) = all.iter_mut().find(|r| norm(&r.text) == norm(t)) {
        r.pinned = true;
        r.text = t.to_string();
    } else {
        all.push(Remark { text: t.to_string(), count: 0, pinned: true, codes: vec![], last: super::core::now_s() });
    }
    remarks_save(dir, &all);
    Ok(())
}

pub fn remark_delete(dir: &Path, text: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = remarks_load(dir);
    all.retain(|r| norm(&r.text) != norm(text));
    remarks_save(dir, &all);
}

/// Замечания для подсказки: закреплённые; затем встречавшиеся 2+ раза — сначала
/// по этому заданию, потом остальные.
pub fn remarks_list(dir: &Path, code: &str, limit: usize, all_items: bool) -> Vec<Remark> {
    let mut all = remarks_load(dir);
    if !all_items {
        all.retain(|r| r.pinned || r.count >= 2);
    }
    all.sort_by(|a, b| {
        let ca = !code.is_empty() && a.codes.iter().any(|c| c == code);
        let cb = !code.is_empty() && b.codes.iter().any(|c| c == code);
        b.pinned.cmp(&a.pinned).then(cb.cmp(&ca)).then(b.count.cmp(&a.count)).then(b.last.cmp(&a.last))
    });
    all.truncate(limit);
    all
}

pub fn remark_json(r: &Remark) -> Json {
    Json::obj().with("text", r.text.as_str()).with("count", r.count).with("pinned", r.pinned).with("codes", r.codes.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pv_lessons_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lessons_pick_same_task_and_similar() {
        let d = dir("pick");
        let mk = |key: &str, code: &str, text: &str, t: f64| Lesson { key: key.into(), code: code.into(), variant: "1".into(), model_grade: 5.0, teacher_grade: t, why: format!("причина {}", key), model_said: String::new(), sketch: sketch(text), date: 1 };
        lesson_put(&d, mk("a", "ПР1", "процессор оперативная память шина данных устройство ввода вывода", 4.0));
        lesson_put(&d, mk("b", "ПР1", "уравнение решение корень проверка подстановка ответ", 3.0));
        lesson_put(&d, mk("c", "ПР2", "процессор оперативная память шина данных устройство", 4.0));
        let got = lessons_for(&d, "ПР1", "1", "x", "процессор и оперативная память, шина данных", 1);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, "a");
        let all = lessons_for(&d, "ПР1", "1", "a", "что угодно", 5);
        assert_eq!(all.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), vec!["b"]);
        let p = lessons_prompt(&got, |g| format!("{}", g));
        assert!(p.contains("преподаватель — 4") && p.contains("причина a"));
        lesson_forget(&d, "a");
        assert_eq!(lessons_load(&d).len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn remarks_from_edits() {
        let d = dir("rem");
        let tpl = "Оценка: 4.\nЗадание 1 выполнено верно.";
        remarks_learn(&d, "ПР1", "Оценка: 4.\nЗадание 1 выполнено верно.\nНет вывода по работе. Не подписаны оси графика.\nМарфа, молодец.\nТихон, хорошо поработал.", tpl, "Лесовикова Марфа");
        remarks_learn(&d, "ПР2", "Нет вывода по работе.", "", "Громобоев Тихон");
        let all = remarks_list(&d, "ПР1", 10, true);
        let texts: Vec<&str> = all.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"Нет вывода по работе."), "{:?}", texts);
        assert!(texts.contains(&"Не подписаны оси графика."));
        assert!(!texts.iter().any(|t| t.contains("Марфа") || t.contains("Тихон")), "ФИО и обращения не попадают в банк: {:?}", texts);
        assert!(!texts.iter().any(|t| t.starts_with("Задание 1 выполнено")), "фраза из шаблона — не правка");
        // в подсказку — повторявшиеся и закреплённые
        let top = remarks_list(&d, "ПР1", 10, false);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].count, 2);
        remark_add(&d, "Оформите титульный лист").unwrap();
        assert!(remarks_list(&d, "", 10, false)[0].pinned);
        remark_delete(&d, "оформите титульный лист");
        assert!(remarks_list(&d, "", 10, false).iter().all(|r| !r.pinned));
        let _ = std::fs::remove_dir_all(&d);
    }
}
