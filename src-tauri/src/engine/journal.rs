//! Журнал группы за семестр: списки студентов, оценки по всем работам (ПР1…ПРn).
//! Хранится в `journal.json` рядом с настройками и не зависит от текущей сессии.

use super::json::{self, Json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub group: String,
    pub student: String,
    pub code: String,
    pub grade: Option<f64>,
    pub violation: String,
    pub date: u64,
    pub file: String,
    pub variant: String,
    pub comment: String,
    /// признаки стиля работы (`detect::style_features`) — для сравнения со следующими работами
    pub style: Vec<f64>,
    /// пометка: «ai_text» — в отзыве (например, из Moodle) упоминается ИИ или нейросеть
    pub flag: String,
}

/// Тема журнала (столбец) в группе: вид, предмет, семестр, когда выдана и до какого срока.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Topic {
    /// "" — определить по названию, "work" — работа, "test" — тест, "custom" — свой столбец
    /// (пропуски, посещения…): в средний балл и долги не входит
    pub kind: String,
    pub title: String,
    pub subject: String,
    /// "" — по дате выдачи или первой оценки
    pub semester: String,
    /// дата выдачи «2026-09-12»
    pub issued: String,
    /// срок сдачи «2026-09-20T23:59» или ""
    pub deadline: String,
    pub note: String,
    /// единица своего столбца: «пар», «ч»
    pub unit: String,
    /// короткое имя для шапки журнала («Тест Р1» вместо длинного названия теста)
    pub short: String,
    /// заголовок элемента в Moodle («Задание:Практическая работа 1») — под ним оценки уходят обратно
    pub moodle: String,
    /// шкала элемента в Moodle («100»), если не 5
    pub mscale: String,
}

impl Topic {
    pub fn to_json(&self) -> Json {
        Json::obj()
            .with("kind", self.kind.as_str())
            .with("title", self.title.as_str())
            .with("subject", self.subject.as_str())
            .with("semester", self.semester.as_str())
            .with("issued", self.issued.as_str())
            .with("deadline", self.deadline.as_str())
            .with("note", self.note.as_str())
            .with("unit", self.unit.as_str())
            .with("short", self.short.as_str())
            .with("moodle", self.moodle.as_str())
            .with("mscale", self.mscale.as_str())
    }
    pub fn from_json(j: &Json) -> Topic {
        let f = |k: &str| j.get(k).str_or("").trim().to_string();
        Topic { kind: f("kind"), title: f("title"), subject: f("subject"), semester: f("semester"), issued: f("issued"), deadline: f("deadline"), note: f("note"), unit: f("unit"), short: f("short"), moodle: f("moodle"), mscale: f("mscale") }
    }
    pub fn is_empty(&self) -> bool {
        *self == Topic::default()
    }
}

/// Короткое имя для шапки журнала, если шифр длинный: «Тестирование по Разделу 1. …» → «Тест Р1»,
/// «Итоговый тест» → «Итог. тест». Пустая строка — сокращать не нужно или не получается.
pub fn auto_short(code: &str, title: &str) -> String {
    if code.chars().count() <= 14 {
        return String::new();
    }
    let src = if title.is_empty() { code } else { title };
    let low = src.to_lowercase().replace('ё', "е");
    let is_test = auto_kind(code, title) == "test";
    // номер после «раздел», «модуль», «тема»
    let num_after = |w: &str| -> Option<String> {
        let i = low.find(w)? + w.len();
        let rest: String = low[i..].chars().skip_while(|c| c.is_alphabetic() || *c == ' ' || *c == '№').take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let rest = rest.trim_end_matches('.').to_string();
        if rest.is_empty() { None } else { Some(rest) }
    };
    for (w, short) in [("раздел", "Р"), ("модул", "М"), ("тема", "Т"), ("глав", "Гл")] {
        if let Some(n) = num_after(w) {
            return if is_test { format!("Тест {}{}", short, n) } else { format!("{}{}", short, n) };
        }
    }
    if is_test {
        let pre = if low.contains("общий итогов") {
            "Общий итог. тест"
        } else if low.contains("итогов") {
            "Итог. тест"
        } else if low.contains("ознакомит") {
            "Ознак. тест"
        } else if low.contains("семестров") {
            "Сем. тест"
        } else if low.contains("входн") {
            "Входной тест"
        } else if low.contains("промежуточн") {
            "Промеж. тест"
        } else {
            ""
        };
        return pre.to_string();
    }
    String::new()
}

/// Вид темы по названию: «Тестирование по разделу 2», «Итоговый тест», «Quiz 1» — тест.
pub fn auto_kind(code: &str, title: &str) -> &'static str {
    let t = format!("{} {}", code, title).to_lowercase();
    if t.contains("тест") || t.contains("test") || t.contains("quiz") || t.contains("опрос") || t.contains("зачёт по тест") {
        "test"
    } else {
        "work"
    }
}

/// Слова в отзыве, по которым оценка помечается «упоминается ИИ».
pub fn mentions_ai(text: &str) -> bool {
    let low = format!(" {} ", text.to_lowercase().replace('ё', "е"));
    let words = [" ии ", " ии,", " ии.", "(ии", " ии)", " ии!", "нейросет", "нейронн", "chatgpt", "chat gpt", "gpt", "искусственн", "сгенерир", "генерац", "deepseek", "gigachat", "гигачат", "yandexgpt", "алиса", " ai ", "claude", "gemini", "copilot", "чат-бот", "чатбот", "чат бот"];
    words.iter().any(|w| low.contains(w))
}

/// Отзыв говорит, что в работе не указан вариант: «Вариант?», «Какой вариант?», «вариант не указан».
pub fn mentions_no_variant(text: &str) -> bool {
    let low = text.to_lowercase().replace('ё', "е");
    let compact: String = low.split_whitespace().collect::<Vec<_>>().join(" ");
    ["вариант?", "вариант ?", "какой вариант", "вариант не указан", "не указан вариант", "не указали вариант", "не указал вариант", "не указала вариант", "нет варианта", "без варианта", "номер варианта?", "укажите вариант", "вариант не подписан"]
        .iter()
        .any(|w| compact.contains(w))
}

/// Пометки оценки по отзыву: «ai_text» (упоминается ИИ), «no_variant» (не указан вариант), через запятую.
pub fn comment_flags(text: &str) -> String {
    let mut f: Vec<&str> = Vec::new();
    if mentions_ai(text) {
        f.push("ai_text");
    }
    if mentions_no_variant(text) {
        f.push("no_variant");
    }
    f.join(",")
}

/// «2026-09-20T23:59» / «2026-09-20» → секунды (как будто UTC, без часового пояса).
pub fn parse_local(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 10 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let (hh, mm) = if s.len() >= 16 { (s.get(11..13)?.parse().ok()?, s.get(14..16)?.parse().ok()?) } else { (23i64, 59i64) };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // дни от 1970-01-01 (алгоритм Хиннанта)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60)
}

/// Семестр по дате «2026-10-05»: сентябрь–январь — 1-й, февраль–август — 2-й семестр учебного года.
pub fn semester_of_date(ymd: &str) -> String {
    let (Some(y), Some(m)) = (ymd.get(0..4).and_then(|x| x.parse::<i64>().ok()), ymd.get(5..7).and_then(|x| x.parse::<i64>().ok())) else { return String::new() };
    match m {
        9..=12 => format!("1 семестр {}/{}", y, (y + 1) % 100),
        1 => format!("1 семестр {}/{}", y - 1, y % 100),
        _ => format!("2 семестр {}/{}", y - 1, y % 100),
    }
}

impl Entry {
    fn to_json(&self) -> Json {
        Json::obj()
            .with("group", self.group.as_str())
            .with("student", self.student.as_str())
            .with("code", self.code.as_str())
            .with("grade", self.grade)
            .with("violation", self.violation.as_str())
            .with("date", self.date)
            .with("file", self.file.as_str())
            .with("variant", self.variant.as_str())
            .with("comment", self.comment.as_str())
            .with("style", Json::Arr(self.style.iter().map(|v| Json::Num((v * 1000.0).round() / 1000.0)).collect()))
            .with("flag", self.flag.as_str())
    }
    fn from_json(j: &Json) -> Entry {
        Entry {
            group: j.get("group").str_or(""),
            student: j.get("student").str_or(""),
            code: j.get("code").str_or(""),
            grade: j.get("grade").as_f64(),
            violation: j.get("violation").str_or(""),
            date: j.get("date").i64_or(0) as u64,
            file: j.get("file").str_or(""),
            variant: j.get("variant").str_or(""),
            comment: j.get("comment").str_or(""),
            style: j.get("style").as_arr().iter().filter_map(|v| v.as_f64()).collect(),
            flag: j.get("flag").str_or(""),
        }
    }
}

/// Учебная группа: название (как папка с её работами) и направление обучения.
#[derive(Clone, Debug, Default)]
pub struct GroupInfo {
    pub name: String,
    /// СПО-9, СПО-11, Специалитет, Бакалавриат, Магистратура или своё
    pub program: String,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct Journal {
    pub path: PathBuf,
    pub groups: Vec<GroupInfo>,
    /// группа → список студентов (порядок = номер варианта, если так принято)
    pub rosters: BTreeMap<String, Vec<String>>,
    pub entries: Vec<Entry>,
    pub variant_is_number: bool,
    /// группа → ФИО → вариант, назначенный преподавателем (по списку, вручную, случайно)
    pub assigned: BTreeMap<String, BTreeMap<String, String>>,
    /// группа → ФИО → номер пары (подгруппы): у напарников одинаковый номер
    pub teams: BTreeMap<String, BTreeMap<String, String>>,
    /// группа → ФИО → почта из Moodle (для выгрузки оценок обратно в Moodle)
    pub emails: BTreeMap<String, BTreeMap<String, String>>,
    /// группа → код → тема (вид, предмет, семестр, даты) — это же календарь заданий
    pub topics: BTreeMap<String, BTreeMap<String, Topic>>,
    /// группа → свои средние: (название, коды тем)
    pub averages: BTreeMap<String, Vec<(String, Vec<String>)>>,
}

impl Journal {
    pub fn load(dir: &Path) -> Journal {
        let path = dir.join("journal.json");
        let mut j = Journal { path: path.clone(), variant_is_number: true, ..Default::default() };
        if let Some(doc) = std::fs::read_to_string(&path).ok().and_then(|s| json::parse(&s).ok()) {
            for (g, list) in doc.get("rosters").as_obj() {
                j.rosters.insert(g.clone(), list.str_list());
            }
            j.entries = doc.get("entries").as_arr().iter().map(Entry::from_json).collect();
            j.variant_is_number = doc.get("variant_is_number").bool_or(true);
            for (key, target) in [("assigned", &mut j.assigned), ("teams", &mut j.teams), ("emails", &mut j.emails)] {
                for (g, m) in doc.get(key).as_obj() {
                    let map: BTreeMap<String, String> = m.as_obj().iter().map(|(k, v)| (k.clone(), v.str_or(""))).filter(|(_, v)| !v.is_empty()).collect();
                    if !map.is_empty() {
                        target.insert(g.clone(), map);
                    }
                }
            }
            for (g, m) in doc.get("topics").as_obj() {
                let map: BTreeMap<String, Topic> = m.as_obj().iter().map(|(k, v)| (k.clone(), Topic::from_json(v))).collect();
                if !map.is_empty() {
                    j.topics.insert(g.clone(), map);
                }
            }
            for (g, list) in doc.get("averages").as_obj() {
                let v: Vec<(String, Vec<String>)> = list.as_arr().iter().map(|a| (a.get("name").str_or(""), a.get("codes").str_list())).filter(|(n, c)| !n.is_empty() && !c.is_empty()).collect();
                if !v.is_empty() {
                    j.averages.insert(g.clone(), v);
                }
            }
            j.groups = doc
                .get("groups")
                .as_arr()
                .iter()
                .map(|g| GroupInfo { name: g.get("name").str_or(""), program: g.get("program").str_or(""), note: g.get("note").str_or("") })
                .filter(|g| !g.name.is_empty())
                .collect();
        }
        j
    }

    pub fn save(&self) {
        let mut ro = Json::obj();
        for (g, l) in &self.rosters {
            ro.set(g, l.clone());
        }
        let to_json = |src: &BTreeMap<String, BTreeMap<String, String>>| {
            let mut out = Json::obj();
            for (g, m) in src {
                let mut o = Json::obj();
                for (k, v) in m {
                    o.set(k, v.as_str());
                }
                out.set(g, o);
            }
            out
        };
        let mut topics = Json::obj();
        for (g, m) in &self.topics {
            let mut o = Json::obj();
            for (k, t) in m {
                if !t.is_empty() {
                    o.set(k, t.to_json());
                }
            }
            topics.set(g, o);
        }
        let mut avgs = Json::obj();
        for (g, list) in &self.averages {
            avgs.set(g, Json::Arr(list.iter().map(|(n, c)| Json::obj().with("name", n.as_str()).with("codes", c.clone())).collect()));
        }
        let doc = Json::obj()
            .with("version", 2)
            .with("topics", topics)
            .with("averages", avgs)
            .with("assigned", to_json(&self.assigned))
            .with("teams", to_json(&self.teams))
            .with("emails", to_json(&self.emails))
            .with("variant_is_number", self.variant_is_number)
            .with("rosters", ro)
            .with(
                "groups",
                Json::Arr(self.groups.iter().map(|g| Json::obj().with("name", g.name.as_str()).with("program", g.program.as_str()).with("note", g.note.as_str())).collect()),
            )
            .with("entries", Json::Arr(self.entries.iter().map(|e| e.to_json()).collect()));
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, doc.pretty()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// Тема группы (пустая, если не задана).
    pub fn topic(&self, group: &str, code: &str) -> Topic {
        self.topics.get(group).and_then(|m| m.get(code)).cloned().unwrap_or_default()
    }

    /// Вид темы: work | test | custom.
    pub fn kind_of(&self, group: &str, code: &str) -> String {
        let t = self.topic(group, code);
        if !t.kind.is_empty() {
            return t.kind;
        }
        auto_kind(code, &t.title).to_string()
    }

    pub fn set_topic(&mut self, group: &str, code: &str, t: Topic) {
        let m = self.topics.entry(group.to_string()).or_default();
        if t.is_empty() {
            m.remove(code);
        } else {
            m.insert(code.to_string(), t);
        }
    }

    /// Дополнить тему, не затирая заданное вручную (импорт, запись проверки).
    pub fn touch_topic(&mut self, group: &str, code: &str, kind: &str, title: &str, subject: &str) {
        let mut t = self.topic(group, code);
        let before = t.clone();
        if t.kind.is_empty() && !kind.is_empty() && kind != auto_kind(code, title) {
            t.kind = kind.to_string();
        }
        if t.title.is_empty() && !title.is_empty() && title != code {
            t.title = title.to_string();
        }
        if t.subject.is_empty() && !subject.is_empty() {
            t.subject = subject.to_string();
        }
        if t != before {
            self.set_topic(group, code, t);
        }
    }

    /// Тема пришла из Moodle: запомнить заголовок и шкалу элемента (для выгрузки обратно)
    /// и придумать короткое имя, если шифр длинный.
    pub fn note_moodle(&mut self, group: &str, code: &str, header: &str, scale: f64) {
        let mut t = self.topic(group, code);
        let before = t.clone();
        if !header.is_empty() {
            t.moodle = header.to_string();
        }
        if scale > 0.0 && (scale - 5.0).abs() > 1e-9 {
            t.mscale = super::core::fmt_grade(scale).replace(',', ".");
        }
        if t.short.is_empty() {
            t.short = auto_short(code, &t.title);
        }
        if t != before {
            self.set_topic(group, code, t);
        }
    }

    /// Все коды тем группы: из оценок и из календаря.
    pub fn codes_of(&self, group: &str) -> Vec<String> {
        let mut c: Vec<String> = self.entries.iter().filter(|e| e.group == group).map(|e| e.code.clone()).collect();
        if let Some(m) = self.topics.get(group) {
            c.extend(m.keys().cloned());
        }
        c.sort();
        c.dedup();
        c
    }

    /// Удалить тему группы целиком: оценки, тему календаря, упоминания в своих средних.
    pub fn delete_topic(&mut self, group: &str, code: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| !(e.group == group && e.code == code));
        if let Some(m) = self.topics.get_mut(group) {
            m.remove(code);
        }
        if let Some(list) = self.averages.get_mut(group) {
            for (_, c) in list.iter_mut() {
                c.retain(|x| x != code);
            }
            list.retain(|(_, c)| !c.is_empty());
        }
        before - self.entries.len()
    }

    /// Переименовать тему группы (код столбца). Если такая уже есть — оценки объединяются.
    pub fn rename_topic(&mut self, group: &str, from: &str, to: &str) {
        if from == to || to.trim().is_empty() {
            return;
        }
        let taken: Vec<String> = self.entries.iter().filter(|e| e.group == group && e.code == to).map(|e| e.student.clone()).collect();
        self.entries.retain(|e| !(e.group == group && e.code == from && taken.contains(&e.student)));
        for e in self.entries.iter_mut() {
            if e.group == group && e.code == from {
                e.code = to.to_string();
            }
        }
        if let Some(m) = self.topics.get_mut(group) {
            if let Some(t) = m.remove(from) {
                m.entry(to.to_string()).or_insert(t);
            }
        }
        if let Some(list) = self.averages.get_mut(group) {
            for (_, c) in list.iter_mut() {
                for x in c.iter_mut() {
                    if x == from {
                        *x = to.to_string();
                    }
                }
                c.dedup();
            }
        }
    }

    /// «2» всем студентам списка, у кого нет оценки по теме (или по всем работам и тестам группы).
    pub fn fill_missing(&mut self, group: &str, codes: &[String], grade: f64, now: u64) -> usize {
        let roster = self.rosters.get(group).cloned().unwrap_or_default();
        let mut students = roster.clone();
        for e in self.entries.iter().filter(|e| e.group == group) {
            if !students.contains(&e.student) {
                students.push(e.student.clone());
            }
        }
        let mut n = 0;
        for c in codes {
            if self.kind_of(group, c) == "custom" {
                continue;
            }
            for s in &students {
                if self.entries.iter().any(|e| e.group == group && &e.student == s && &e.code == c && e.grade.is_some()) {
                    continue;
                }
                self.upsert(Entry { group: group.into(), student: s.clone(), code: c.clone(), grade: Some(grade), date: now, file: "не сдано".into(), comment: "работа не сдана".into(), ..Default::default() });
                n += 1;
            }
        }
        n
    }

    /// Группа есть в реестре (её видно в «Группы и направления» на всех шагах).
    pub fn ensure_group(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.groups.iter().any(|g| g.name == name) {
            self.groups.push(GroupInfo { name: name.to_string(), ..Default::default() });
        }
    }

    /// Удалить группу: реестр, список, варианты, пары, почту, темы и (по желанию) оценки.
    pub fn delete_group(&mut self, name: &str, with_entries: bool) {
        self.groups.retain(|g| g.name != name);
        self.rosters.remove(name);
        self.assigned.remove(name);
        self.teams.remove(name);
        if with_entries {
            self.emails.remove(name);
            self.topics.remove(name);
            self.averages.remove(name);
            self.entries.retain(|e| e.group != name);
        }
    }

    /// Номер пары студента в группе (если пары заданы).
    pub fn team_of(&self, group: &str, student: &str) -> Option<String> {
        self.teams.get(group).and_then(|m| m.get(student)).filter(|t| !t.is_empty()).cloned()
    }

    /// Убрать студента из группы: список, варианты, пары и все его оценки.
    pub fn delete_student(&mut self, group: &str, student: &str) -> usize {
        if let Some(l) = self.rosters.get_mut(group) {
            l.retain(|n| n != student);
        }
        for m in [&mut self.assigned, &mut self.teams, &mut self.emails] {
            if let Some(x) = m.get_mut(group) {
                x.remove(student);
            }
        }
        let before = self.entries.len();
        self.entries.retain(|e| !(e.group == group && e.student == student));
        before - self.entries.len()
    }

    /// Переименовать студента (или объединить с другим): оценки переходят к новому имени.
    pub fn rename_student(&mut self, group: &str, from: &str, to: &str) {
        if from == to || to.trim().is_empty() {
            return;
        }
        let exists = self.rosters.get(group).map(|l| l.iter().any(|n| n == to)).unwrap_or(false);
        if let Some(l) = self.rosters.get_mut(group) {
            if exists {
                l.retain(|n| n != from);
            } else if let Some(x) = l.iter_mut().find(|n| *n == from) {
                *x = to.to_string();
            }
        }
        for m in [&mut self.assigned, &mut self.teams, &mut self.emails] {
            if let Some(x) = m.get_mut(group) {
                if let Some(v) = x.remove(from) {
                    x.entry(to.to_string()).or_insert(v);
                }
            }
        }
        // оценки: при совпадении работы у обоих остаётся запись нового имени
        let taken: Vec<String> = self.entries.iter().filter(|e| e.group == group && e.student == to).map(|e| e.code.clone()).collect();
        self.entries.retain(|e| !(e.group == group && e.student == from && taken.contains(&e.code)));
        for e in self.entries.iter_mut() {
            if e.group == group && e.student == from {
                e.student = to.to_string();
            }
        }
    }

    /// Направление группы (если группа зарегистрирована).
    pub fn program_of(&self, group: &str) -> String {
        self.groups.iter().find(|g| g.name == group).map(|g| g.program.clone()).unwrap_or_default()
    }

    /// Переименование группы везде: реестр, список студентов, журнал.
    pub fn rename_group(&mut self, old: &str, new: &str) {
        if old == new || old.is_empty() {
            return;
        }
        for g in self.groups.iter_mut() {
            if g.name == old {
                g.name = new.to_string();
            }
        }
        if let Some(r) = self.rosters.remove(old) {
            self.rosters.insert(new.to_string(), r);
        }
        if let Some(a) = self.assigned.remove(old) {
            self.assigned.insert(new.to_string(), a);
        }
        if let Some(a) = self.teams.remove(old) {
            self.teams.insert(new.to_string(), a);
        }
        if let Some(a) = self.emails.remove(old) {
            self.emails.insert(new.to_string(), a);
        }
        if let Some(a) = self.topics.remove(old) {
            self.topics.insert(new.to_string(), a);
        }
        if let Some(a) = self.averages.remove(old) {
            self.averages.insert(new.to_string(), a);
        }
        for e in self.entries.iter_mut() {
            if e.group == old {
                e.group = new.to_string();
            }
        }
    }

    pub fn upsert(&mut self, e: Entry) {
        if let Some(x) = self.entries.iter_mut().find(|x| x.group == e.group && x.student == e.student && x.code == e.code) {
            *x = e;
        } else {
            self.entries.push(e);
        }
    }

    /// Кто сдал эту работу: (имя, найдено ли в списке группы).
    pub fn student_for(&self, group: &str, file_name: &str, variant: Option<&str>) -> (String, bool) {
        let low = file_name.to_lowercase().replace('ё', "е");
        if let Some(list) = self.rosters.get(group) {
            // 1) фамилия из списка встречается в имени файла
            for name in list {
                let sur = name.split_whitespace().next().unwrap_or("").to_lowercase().replace('ё', "е");
                if sur.chars().count() >= 3 && low.contains(&sur) {
                    return (name.clone(), true);
                }
            }
            // 1б) список вида «Имя Фамилия»: любое слово ФИО, которое есть только у одного
            // студента группы (в том числе латиницей: «veterkova»)
            let toks = super::roster::tokens(&Path::new(file_name).with_extension("").to_string_lossy());
            let hits: Vec<&String> = list.iter().filter(|n| super::roster::mentioned(n, list, &toks)).collect();
            if hits.len() == 1 {
                return (hits[0].clone(), true);
            }
            // 2) вариант назначен преподавателем: единственный студент с этим вариантом
            if let (Some(v), Some(map)) = (variant, self.assigned.get(group)) {
                let who: Vec<&String> = map.iter().filter(|(n, x)| x.as_str() == v && list.contains(n)).map(|(n, _)| n).collect();
                // один студент или одна пара с этим вариантом
                let team0 = who.first().and_then(|n| self.team_of(group, n));
                if who.len() == 1 || (who.len() > 1 && team0.is_some() && who.iter().all(|n| self.team_of(group, n) == team0)) {
                    return (who[0].clone(), true);
                }
            }
            // 3) номер варианта = номер в списке (если варианты не назначены вручную)
            if self.variant_is_number && self.assigned.get(group).map(|m| m.is_empty()).unwrap_or(true) {
                if let Some(n) = variant.and_then(|v| v.parse::<usize>().ok()) {
                    if n >= 1 && n <= list.len() {
                        return (list[n - 1].clone(), true);
                    }
                }
            }
        }
        // 3) фамилия из имени файла («ПР2_Лесовиков_В1.docx»)
        if let Some(s) = surname_from_file(file_name) {
            if let Some(list) = self.rosters.get(group) {
                if let Some(name) = list.iter().find(|n| n.to_lowercase().starts_with(&s.to_lowercase())) {
                    return (name.clone(), true);
                }
            }
            return (s, false);
        }
        // 4) имя файла без пометок в скобках: «Вариант 9В (позже сроков)» → «Вариант 9В»
        let stem = Path::new(file_name).with_extension("").to_string_lossy().to_string();
        let mut clean = String::new();
        let mut depth = 0;
        for c in stem.chars() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = (depth - 1).max(0),
                _ if depth == 0 => clean.push(c),
                _ => {}
            }
        }
        let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
        if !clean.is_empty() {
            return (clean, false);
        }
        match variant {
            Some(v) => (format!("Вариант {}", v), false),
            None => (stem, false),
        }
    }
}

/// Фамилия (и, возможно, имя) из имени файла: «ПР2_Лесовиков_Иван_В3.docx» → «Лесовиков Иван».
pub fn surname_from_file(file_name: &str) -> Option<String> {
    let stem = Path::new(file_name).with_extension("").to_string_lossy().to_string();
    let stop = [
        "вариант", "варинт", "вар", "практическая", "практическое", "работа", "задание", "задания", "отчет", "отчёт", "лабораторная", "лр", "пр", "ответы", "ответ", "итоговая", "ведомость", "работы", "позже", "сроков", "срока", "не", "было", "на", "паре", "написан", "максимум", "контрольная", "самостоятельная", "домашнее", "группа", "гр", "копия", "версия", "исправленная", "новая", "final", "doc",
    ];
    let mut out: Vec<String> = Vec::new();
    for tok in stem.split(|c: char| c == '_' || c == '-' || c == ' ' || c == '.' || c == ',' || c == '(' || c == ')' || c == '+') {
        let t = tok.trim();
        if t.is_empty() {
            if !out.is_empty() {
                break;
            }
            continue;
        }
        let low = t.to_lowercase();
        let chars: Vec<char> = t.chars().collect();
        let is_name = chars.len() >= 3
            && chars[0].is_uppercase()
            && chars.iter().all(|c| c.is_alphabetic() || *c == '-')
            && chars[1..].iter().any(|c| c.is_lowercase())
            && !stop.contains(&low.as_str())
            && chars.iter().all(|c| ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(' ')) || *c == 'ё' || *c == 'Ё' || *c == '-');
        if is_name {
            out.push(t.to_string());
            if out.len() == 2 {
                break;
            }
        } else if !out.is_empty() {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(surname_from_file("ПР2_Лесовиков_В1.docx").as_deref(), Some("Лесовиков"));
        assert_eq!(surname_from_file("ПР2_Громобоева Анна_В3.docx").as_deref(), Some("Громобоева Анна"));
        assert_eq!(surname_from_file("Вариант 20.docx"), None);
        assert_eq!(surname_from_file("Вариант 8 (Позже сроков).docx"), None);
        let mut j = Journal::default();
        j.variant_is_number = true;
        j.rosters.insert("1гр".into(), vec!["Арбузов Антон".into(), "Баранкина Вера".into()]);
        assert_eq!(j.student_for("1гр", "Вариант 2.docx", Some("2")).0, "Баранкина Вера");
        assert_eq!(j.student_for("1гр", "ПР3_Арбузов.docx", Some("7")).0, "Арбузов Антон");
        assert_eq!(j.student_for("2гр", "Вариант 5.docx", Some("5")).0, "Вариант 5");
        assert_eq!(j.student_for("2гр", "Вариант 9В (Позже сроков).docx", Some("9")).0, "Вариант 9В");
    }

    #[test]
    fn topics() {
        assert_eq!(auto_kind("Тестирование по разделу 2", ""), "test");
        assert_eq!(auto_kind("ПР3", "Практическая работа 3"), "work");
        assert!(mentions_ai("Работа выполнена с помощью ИИ."));
        assert!(mentions_ai("похоже на ChatGPT"));
        assert!(!mentions_ai("Хорошо, но мало примеров"));
        assert_eq!(semester_of_date("2026-10-05"), "1 семестр 2026/27");
        assert_eq!(semester_of_date("2027-01-20"), "1 семестр 2026/27");
        assert_eq!(semester_of_date("2027-03-01"), "2 семестр 2026/27");
        assert_eq!(super::super::core::ymd(parse_local("2026-09-20T10:30").unwrap()), "2026-09-20");
        assert_eq!(parse_local("2026-09-20T10:30").unwrap() % 86400, 10 * 3600 + 30 * 60);
        let mut j = Journal::default();
        j.rosters.insert("1гр".into(), vec!["Арбузов Антон".into(), "Баранкина Вера".into()]);
        j.upsert(Entry { group: "1гр".into(), student: "Арбузов Антон".into(), code: "ПР1".into(), grade: Some(4.0), ..Default::default() });
        j.upsert(Entry { group: "1гр".into(), student: "Арбузов Антон".into(), code: "ПР2".into(), grade: Some(5.0), ..Default::default() });
        j.set_topic("1гр", "Пропуски", Topic { kind: "custom".into(), unit: "пар".into(), ..Default::default() });
        assert_eq!(j.fill_missing("1гр", &j.codes_of("1гр"), 2.0, 1), 2);
        assert_eq!(j.entries.iter().filter(|e| e.student == "Баранкина Вера").count(), 2);
        j.rename_topic("1гр", "ПР2", "ПР1");
        assert_eq!(j.entries.iter().filter(|e| e.student == "Арбузов Антон").count(), 1);
        assert_eq!(j.delete_topic("1гр", "ПР1"), 2);
        assert_eq!(j.codes_of("1гр"), vec!["Пропуски".to_string()]);
        j.ensure_group("1гр");
        j.delete_group("1гр", true);
        assert!(j.groups.is_empty() && j.topics.is_empty() && j.entries.is_empty());
    }
}
