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
        let doc = Json::obj()
            .with("version", 1)
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
}
