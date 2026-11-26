//! Память между сессиями проверки:
//! * **сверка** — как оценки модели расходятся с итоговыми оценками преподавателя
//!   (`calibration.json`: по записи на работу, без ФИО);
//! * **архив работ прошлых лет** — отпечатки текста (шинглы) уже проверенных работ
//!   (`archive/<id>.txt`), чтобы ловить списывание у прошлогодних студентов.
//!   Сам текст работ в архив не кладётся — только отпечатки и подпись (ФИО, группа, дата).

use super::json::{self, Json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// ------------------------------------------------------------------ сверка

#[derive(Clone, Debug, Default)]
pub struct CalRec {
    /// отпечаток работы (повторная запись той же работы заменяет прежнюю)
    pub key: String,
    pub code: String,
    pub group: String,
    pub model: String,
    /// оценка приложения (модель + правила)
    pub auto: f64,
    /// итоговая оценка (с правкой преподавателя)
    pub final_: f64,
    pub date: u64,
}

fn cal_path(dir: &Path) -> PathBuf {
    dir.join("calibration.json")
}

pub fn cal_load(dir: &Path) -> Vec<CalRec> {
    let Ok(t) = std::fs::read_to_string(cal_path(dir)) else { return Vec::new() };
    let Ok(j) = json::parse(&t) else { return Vec::new() };
    j.as_arr()
        .iter()
        .map(|r| CalRec {
            key: r.get("key").str_or(""),
            code: r.get("code").str_or(""),
            group: r.get("group").str_or(""),
            model: r.get("model").str_or(""),
            auto: r.get("auto").f64_or(0.0),
            final_: r.get("final").f64_or(0.0),
            date: r.get("date").f64_or(0.0) as u64,
        })
        .collect()
}

/// Добавить записи (та же работа — заменить). Хранится не больше 5000 последних.
pub fn cal_add(dir: &Path, recs: Vec<CalRec>) {
    if recs.is_empty() {
        return;
    }
    let mut all = cal_load(dir);
    for r in recs {
        all.retain(|x| !(x.key == r.key && x.code == r.code));
        all.push(r);
    }
    if all.len() > 5000 {
        let cut = all.len() - 5000;
        all.drain(..cut);
    }
    let arr: Vec<Json> = all
        .iter()
        .map(|r| Json::obj().with("key", r.key.as_str()).with("code", r.code.as_str()).with("group", r.group.as_str()).with("model", r.model.as_str()).with("auto", r.auto).with("final", r.final_).with("date", r.date))
        .collect();
    let _ = std::fs::write(cal_path(dir), Json::Arr(arr).to_string());
}

/// Сводка: по каждой работе (шифру) и по каждой модели — сколько раз преподаватель
/// менял оценку и в какую сторону. `code` — только эта работа.
pub fn cal_summary(dir: &Path, code: &str) -> Json {
    let all: Vec<CalRec> = cal_load(dir).into_iter().filter(|r| code.is_empty() || r.code == code).collect();
    let block = |list: &[&CalRec]| -> Json {
        let n = list.len();
        let changed: Vec<&&CalRec> = list.iter().filter(|r| (r.final_ - r.auto).abs() > 0.01).collect();
        let up = changed.iter().filter(|r| r.final_ > r.auto).count();
        let down = changed.len() - up;
        let mean = if n > 0 { list.iter().map(|r| r.final_ - r.auto).sum::<f64>() / n as f64 } else { 0.0 };
        let mean_changed = if changed.is_empty() { 0.0 } else { changed.iter().map(|r| r.final_ - r.auto).sum::<f64>() / changed.len() as f64 };
        Json::obj()
            .with("n", n)
            .with("changed", changed.len())
            .with("up", up)
            .with("down", down)
            .with("mean", (mean * 100.0).round() / 100.0)
            .with("mean_changed", (mean_changed * 100.0).round() / 100.0)
    };
    let mut by_code: Vec<(String, Vec<&CalRec>)> = Vec::new();
    let mut by_model: Vec<(String, Vec<&CalRec>)> = Vec::new();
    for r in &all {
        match by_code.iter_mut().find(|x| x.0 == r.code) {
            Some(x) => x.1.push(r),
            None => by_code.push((r.code.clone(), vec![r])),
        }
        let m = if r.model.is_empty() { "модель не указана".to_string() } else { r.model.clone() };
        match by_model.iter_mut().find(|x| x.0 == m) {
            Some(x) => x.1.push(r),
            None => by_model.push((m, vec![r])),
        }
    }
    by_code.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    by_model.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    // подсказки: модель систематически завышает или занижает
    let mut hints: Vec<Json> = Vec::new();
    for (c, l) in &by_code {
        if l.len() < 5 {
            continue;
        }
        let mean = l.iter().map(|r| r.final_ - r.auto).sum::<f64>() / l.len() as f64;
        let share = l.iter().filter(|r| (r.final_ - r.auto).abs() > 0.01).count() as f64 / l.len() as f64;
        if mean.abs() >= 0.3 && share >= 0.25 {
            let txt = if mean < 0.0 {
                format!("По {} модель в среднем завышает на {} балла: вы снижали оценку в {} % работ. Уточните эталон ответов или критерии (рубрику) — например, за что снимать баллы.", c, fmt(mean.abs()), (share * 100.0).round())
            } else {
                format!("По {} модель в среднем занижает на {} балла: вы повышали оценку в {} % работ. Возможно, эталон слишком строгий — добавьте в ключ допустимые варианты ответов.", c, fmt(mean), (share * 100.0).round())
            };
            hints.push(Json::obj().with("code", c.as_str()).with("text", txt));
        }
    }
    let total = block(&all.iter().collect::<Vec<_>>());
    Json::obj()
        .with("total", total)
        .with("codes", Json::Arr(by_code.iter().map(|(c, l)| block(l).with("code", c.as_str())).collect()))
        .with("models", Json::Arr(by_model.iter().map(|(m, l)| block(l).with("model", m.as_str())).collect()))
        .with("hints", Json::Arr(hints))
}

fn fmt(x: f64) -> String {
    let t = format!("{:.1}", x);
    t.trim_end_matches('0').trim_end_matches('.').replace('.', ",")
}

// ------------------------------------------------------------------ архив прошлых лет

#[derive(Clone, Debug, Default)]
pub struct ArcEntry {
    pub id: String,
    pub code: String,
    pub title: String,
    pub group: String,
    pub student: String,
    pub date: u64,
    /// отпечаток файла — повторно та же работа не добавляется
    pub hash: String,
    pub shingles: Vec<u64>,
}

impl ArcEntry {
    pub fn meta(&self) -> Json {
        Json::obj()
            .with("id", self.id.as_str())
            .with("code", self.code.as_str())
            .with("title", self.title.as_str())
            .with("group", self.group.as_str())
            .with("student", self.student.as_str())
            .with("date", self.date)
            .with("hash", self.hash.as_str())
    }
    /// «Громобоев Тихон, ИС-21, 2025»
    pub fn label(&self) -> String {
        let year = super::report::date_ymd(self.date).get(..4).unwrap_or("").to_string();
        [self.student.as_str(), self.group.as_str(), year.as_str()].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(", ")
    }
}

fn arc_dir(dir: &Path) -> PathBuf {
    dir.join("archive")
}

static CACHE: Mutex<Option<(PathBuf, Vec<ArcEntry>)>> = Mutex::new(None);

/// Все записи архива (с кэшем в памяти).
pub fn arc_load(dir: &Path) -> Vec<ArcEntry> {
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((d, v)) = c.as_ref() {
        if d == dir {
            return v.clone();
        }
    }
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(arc_dir(dir)) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x != "txt").unwrap_or(true) {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(&p) else { continue };
            let mut lines = t.lines();
            let Some(Ok(m)) = lines.next().map(json::parse) else { continue };
            let sh: Vec<u64> = lines.next().unwrap_or("").split_whitespace().filter_map(|x| u64::from_str_radix(x, 16).ok()).collect();
            out.push(ArcEntry {
                id: m.get("id").str_or(""),
                code: m.get("code").str_or(""),
                title: m.get("title").str_or(""),
                group: m.get("group").str_or(""),
                student: m.get("student").str_or(""),
                date: m.get("date").f64_or(0.0) as u64,
                hash: m.get("hash").str_or(""),
                shingles: sh,
            });
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    *c = Some((dir.to_path_buf(), out.clone()));
    out
}

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Положить работы в архив. Та же работа (отпечаток файла + шифр) повторно не кладётся.
/// Возвращает, сколько добавлено.
pub fn arc_add(dir: &Path, entries: Vec<ArcEntry>) -> usize {
    let have: HashSet<(String, String)> = arc_load(dir).into_iter().map(|e| (e.hash, e.code)).collect();
    let d = arc_dir(dir);
    let _ = std::fs::create_dir_all(&d);
    let mut n = 0;
    for (i, mut e) in entries.into_iter().enumerate() {
        if e.shingles.len() < 20 || have.contains(&(e.hash.clone(), e.code.clone())) {
            continue;
        }
        if e.id.is_empty() {
            e.id = format!("a{}{:03}", super::core::now_s(), i);
        }
        let body = format!("{}\n{}\n", e.meta().to_string(), e.shingles.iter().map(|x| format!("{:x}", x)).collect::<Vec<_>>().join(" "));
        if std::fs::write(d.join(format!("{}.txt", e.id)), body).is_ok() {
            n += 1;
        }
    }
    invalidate();
    n
}

/// Убрать записи: по id или целиком работу (шифр).
pub fn arc_delete(dir: &Path, ids: &[String], code: &str) -> usize {
    let mut n = 0;
    for e in arc_load(dir) {
        if ids.contains(&e.id) || (!code.is_empty() && e.code == code) {
            if std::fs::remove_file(arc_dir(dir).join(format!("{}.txt", e.id))).is_ok() {
                n += 1;
            }
        }
    }
    invalidate();
    n
}

/// Сводка архива: по шифру работы — сколько работ и за какие годы.
pub fn arc_summary(dir: &Path) -> Json {
    let all = arc_load(dir);
    let mut by: Vec<(String, String, usize, Vec<String>, Vec<String>)> = Vec::new();
    for e in &all {
        let year = super::report::date_ymd(e.date).get(..4).unwrap_or("").to_string();
        match by.iter_mut().find(|x| x.0 == e.code) {
            Some(x) => {
                x.2 += 1;
                if !x.3.contains(&year) {
                    x.3.push(year);
                }
                if !e.group.is_empty() && !x.4.contains(&e.group) {
                    x.4.push(e.group.clone());
                }
            }
            None => by.push((e.code.clone(), e.title.clone(), 1, vec![year], if e.group.is_empty() { vec![] } else { vec![e.group.clone()] })),
        }
    }
    Json::obj().with("total", all.len()).with(
        "codes",
        Json::Arr(by.into_iter().map(|(c, t, n, y, g)| Json::obj().with("code", c.as_str()).with("title", t.as_str()).with("n", n).with("years", y).with("groups", g)).collect()),
    )
}

/// Совпадения текущих работ с архивом.
/// `works`: (id работы, шифр, отпечаток файла, шинглы без текста заданий).
/// `all_codes` — сравнивать со всем архивом, а не только с той же работой.
/// Ответ: id работы → [(подпись архивной работы, доля совпадения)].
pub fn arc_matches(dir: &Path, works: &[(String, String, String, HashSet<u64>)], threshold: f64, all_codes: bool) -> HashMap<String, Vec<(String, f64)>> {
    let arc = arc_load(dir);
    let mut res: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    if arc.is_empty() || works.is_empty() {
        return res;
    }
    let norm = |c: &str| c.to_lowercase().replace(' ', "");
    let current: HashSet<String> = works.iter().map(|w| w.2.clone()).collect();
    // шаблонные фразы: шинглы, которые есть у многих работ (и текущих, и архивных)
    let mut freq: HashMap<u64, usize> = HashMap::new();
    for (_, _, _, s) in works {
        for h in s {
            *freq.entry(*h).or_default() += 1;
        }
    }
    let arc_sets: Vec<HashSet<u64>> = arc.iter().map(|e| e.shingles.iter().copied().collect()).collect();
    for s in &arc_sets {
        for h in s {
            *freq.entry(*h).or_default() += 1;
        }
    }
    let limit = (((works.len() + arc.len()) as f64) * 0.25).ceil().max(3.0) as usize;
    let clean = |s: &HashSet<u64>| -> HashSet<u64> { s.iter().filter(|h| freq.get(h).copied().unwrap_or(0) < limit).copied().collect() };
    let arc_clean: Vec<HashSet<u64>> = arc_sets.iter().map(clean).collect();
    for (id, code, hash, set) in works {
        let a = clean(set);
        if a.len() < 25 {
            continue;
        }
        for (k, e) in arc.iter().enumerate() {
            // это та же или другая работа текущей проверки (уже лежит в архиве после записи
            // в журнал) — совпадения внутри группы ищет обычная проверка на списывание
            if &e.hash == hash || current.contains(&e.hash) {
                continue;
            }
            if !all_codes && !code.is_empty() && norm(&e.code) != norm(code) {
                continue;
            }
            let b = &arc_clean[k];
            let m = a.len().min(b.len());
            if m < 25 {
                continue;
            }
            let score = a.intersection(b).count() as f64 / m as f64;
            if score >= threshold {
                res.entry(id.clone()).or_default().push((e.label(), (score * 100.0).round() / 100.0));
            }
        }
    }
    for v in res.values_mut() {
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(5);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_and_calibration() {
        let dir = std::env::temp_dir().join(format!("proverka-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = "Протокол TCP обеспечивает надёжную доставку данных с подтверждением и повторной передачей потерянных сегментов, а UDP работает без установления соединения и подходит для потокового видео и голосовой связи, где задержка важнее потерь. Маска подсети 255.255.255.0 означает сеть на 254 узла, широковещательный адрес заканчивается на 255, адрес сети на ноль.";
        let sh: Vec<u64> = super::super::detect::shingle_set(text).into_iter().collect();
        let e = ArcEntry { code: "ПР3".into(), group: "ИС-11".into(), student: "Ирискова Злата".into(), date: 1_730_000_000, hash: "aa".into(), shingles: sh.clone(), ..Default::default() };
        assert_eq!(arc_add(&dir, vec![e.clone()]), 1);
        assert_eq!(arc_add(&dir, vec![e]), 0); // та же работа повторно не кладётся
        let cur: HashSet<u64> = super::super::detect::shingle_set(text);
        let m = arc_matches(&dir, &[("w1".into(), "ПР3".into(), "bb".into(), cur.clone())], 0.6, false);
        assert_eq!(m.get("w1").map(|v| v[0].0.as_str()), Some("Ирискова Злата, ИС-11, 2024"));
        assert!(arc_matches(&dir, &[("w1".into(), "ПР4".into(), "bb".into(), cur)], 0.6, false).is_empty());
        cal_add(&dir, (0..6).map(|i| CalRec { key: format!("k{}", i), code: "ПР3".into(), auto: 5.0, final_: if i < 4 { 4.0 } else { 5.0 }, ..Default::default() }).collect());
        let s = cal_summary(&dir, "");
        assert_eq!(s.get("total").get("changed").i64_or(0), 4);
        assert_eq!(s.get("hints").as_arr().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
