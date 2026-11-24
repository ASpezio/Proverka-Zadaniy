//! Обезличивание для облачных нейросетей (152-ФЗ): перед отправкой ФИО студентов
//! заменяются на «Студент 1», «Студент 2»…, почта и телефоны — на пометки.
//! В ответе модели метки возвращаются к настоящим именам.

use super::roster::{tokens, translit};

#[derive(Clone, Debug, Default)]
pub struct Anon {
    /// (слова ФИО в нижнем регистре, метка)
    people: Vec<(Vec<String>, String, String)>, // (слова, метка, полное имя)
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic()
}

/// Основа слова для склонений: «лесовикова» → «лесовиков», «анна» → «анн».
fn stem(w: &str) -> String {
    let n = w.chars().count();
    if n <= 4 {
        return w.to_string();
    }
    let vowels = "аеёиоуыэюяйь";
    let mut s: Vec<char> = w.chars().collect();
    while s.len() > 4 && s.last().map(|c| vowels.contains(*c)).unwrap_or(false) {
        s.pop();
    }
    s.into_iter().collect()
}

/// Совпадает ли слово с частью ФИО с учётом падежа (до 3 букв окончания) и латиницы.
fn matches_part(word: &str, part: &str) -> bool {
    if word == part {
        return true;
    }
    let pc = part.chars().count();
    if pc < 3 {
        return false;
    }
    let st = stem(part);
    let wc = word.chars().count();
    if pc >= 4 && word.starts_with(&st) && wc <= st.chars().count() + 3 && wc + 1 >= pc {
        return true;
    }
    translit(part).iter().any(|t| t.len() >= 4 && (word == t || (word.starts_with(t.as_str()) && word.len() <= t.len() + 2)))
}

impl Anon {
    pub fn new(names: &[String]) -> Anon {
        let mut people = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for n in names {
            let key = n.trim().to_lowercase();
            if key.is_empty() || seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let words: Vec<String> = tokens(n).into_iter().filter(|w| w.chars().count() >= 2 && !w.chars().all(|c| c.is_ascii_digit())).collect();
            if words.is_empty() {
                continue;
            }
            let label = format!("Студент {}", people.len() + 1);
            people.push((words, label, n.trim().to_string()));
        }
        Anon { people }
    }

    pub fn len(&self) -> usize {
        self.people.len()
    }

    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    /// Кто из людей назван этим словом: фамилия (1-е слово) — сама по себе,
    /// имя и отчество — только рядом с другим словом того же человека.
    fn who(&self, word: &str, prev: Option<usize>, next_word: Option<&str>) -> Option<usize> {
        let w = word.to_lowercase().replace('ё', "е");
        for (i, (parts, _, _)) in self.people.iter().enumerate() {
            if parts.first().map(|s| matches_part(&w, s)).unwrap_or(false) && parts[0].chars().count() >= 4 {
                return Some(i);
            }
        }
        for (i, (parts, _, _)) in self.people.iter().enumerate() {
            if parts.iter().skip(1).any(|p| matches_part(&w, p)) {
                let near_prev = prev == Some(i);
                let near_next = next_word.map(|n| {
                    let n = n.to_lowercase().replace('ё', "е");
                    parts.iter().any(|p| matches_part(&n, p))
                });
                if near_prev || near_next.unwrap_or(false) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Заменить ФИО, почту и телефоны.
    pub fn apply(&self, text: &str) -> String {
        let text = mask_contacts(text);
        if self.people.is_empty() {
            return text;
        }
        // разбиваем на слова и разделители
        let mut segs: Vec<(bool, String)> = Vec::new();
        for c in text.chars() {
            let w = is_word_char(c);
            match segs.last_mut() {
                Some((lw, s)) if *lw == w => s.push(c),
                _ => segs.push((w, c.to_string())),
            }
        }
        let words: Vec<usize> = segs.iter().enumerate().filter(|(_, s)| s.0).map(|(i, _)| i).collect();
        let mut owner: Vec<Option<usize>> = vec![None; segs.len()];
        let mut prev: Option<usize> = None;
        for (k, &i) in words.iter().enumerate() {
            let next = words.get(k + 1).map(|&j| segs[j].1.as_str());
            // соседние слова — только через пробел, «_», «-» или «.»
            let gap_ok = k > 0 && segs[words[k - 1] + 1..i].iter().all(|s| s.1.chars().all(|c| c == ' ' || c == '_' || c == '-' || c == '.'));
            let o = self.who(&segs[i].1, if gap_ok { prev } else { None }, next);
            owner[i] = o;
            prev = o;
        }
        // инициалы рядом с найденной фамилией: «Лесовиков И.И.», «И. И. Лесовиков»
        for (k, &i) in words.iter().enumerate() {
            if owner[i].is_some() || segs[i].1.chars().count() != 1 || !segs[i].1.chars().all(|c| c.is_uppercase()) {
                continue;
            }
            let dot_after = segs.get(i + 1).map(|s| s.1.starts_with('.')).unwrap_or(false);
            if !dot_after {
                continue;
            }
            let mut j = k;
            let mut found = None;
            // ищем фамилию в пределах двух слов слева или справа
            for d in 1..=2 {
                if j >= d {
                    if let Some(o) = owner[words[j - d]] {
                        found = Some(o);
                        break;
                    }
                }
                if let Some(&r) = words.get(k + d) {
                    if let Some(o) = owner[r] {
                        found = Some(o);
                        break;
                    }
                }
            }
            if found.is_none() {
                continue;
            }
            j = i;
            owner[j] = found;
        }
        // склеиваем: подряд идущие слова одного человека — одна метка
        let mut out = String::new();
        let mut i = 0;
        while i < segs.len() {
            if let Some(o) = owner[i] {
                let mut j = i;
                // поглощаем «слово [разделитель] слово/инициал» того же человека
                loop {
                    let mut k = j + 1;
                    while k < segs.len() && !segs[k].0 && segs[k].1.chars().all(|c| c == ' ' || c == '.' || c == '_') && segs[k].1.chars().count() <= 3 {
                        k += 1;
                    }
                    if k < segs.len() && segs[k].0 && owner[k] == Some(o) {
                        j = k;
                    } else {
                        break;
                    }
                }
                out.push_str(&self.people[o].1);
                // точка после последнего инициала
                if segs[j].1.chars().count() == 1 && segs.get(j + 1).map(|s| s.1.starts_with('.')).unwrap_or(false) {
                    let rest: String = segs[j + 1].1.chars().skip(1).collect();
                    out.push_str(&rest);
                    i = j + 2;
                    continue;
                }
                i = j + 1;
            } else {
                out.push_str(&segs[i].1);
                i += 1;
            }
        }
        out
    }

    /// Вернуть настоящие имена в ответ модели: «Студент 2», «Студента 2», «Студенту 2»…
    pub fn restore(&self, text: &str) -> String {
        let mut s = text.to_string();
        // сначала большие номера: «Студент 12» раньше «Студент 1»
        for (_, label, name) in self.people.iter().rev() {
            let num = label.trim_start_matches("Студент ");
            for form in ["Студентом", "Студенту", "Студента", "Студенте", "Студент", "студентом", "студенту", "студента", "студенте", "студент"] {
                let pat = format!("{} {}", form, num);
                let mut res = String::new();
                let mut rest = s.as_str();
                while let Some(p) = rest.find(&pat) {
                    let after = &rest[p + pat.len()..];
                    // «Студент 1» не должен съесть «Студент 12»
                    if after.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        res.push_str(&rest[..p + pat.len()]);
                    } else {
                        res.push_str(&rest[..p]);
                        res.push_str(name);
                    }
                    rest = after;
                }
                res.push_str(rest);
                s = res;
            }
        }
        s
    }
}

/// Почта → [почта], телефон (10+ цифр) → [телефон].
pub fn mask_contacts(text: &str) -> String {
    let mut out = String::new();
    for (k, part) in text.split(' ').enumerate() {
        if k > 0 {
            out.push(' ');
        }
        let core = part.trim_matches(|c: char| ",;:()<>«»\"'".contains(c));
        if let Some(at) = core.find('@') {
            if at > 0 && core[at..].contains('.') && !core.contains(char::is_whitespace) {
                out.push_str(&part.replace(core, "[почта]"));
                continue;
            }
        }
        out.push_str(part);
    }
    // телефоны: +7 (912) 345-67-89, 89123456789
    let chars: Vec<char> = out.chars().collect();
    let mut res = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '+' || c.is_ascii_digit() || (c == '(' && chars.get(i + 1).map(|x| x.is_ascii_digit()).unwrap_or(false)) {
            let mut j = i;
            let mut digits = 0;
            while j < chars.len() && (chars[j].is_ascii_digit() || " -()+".contains(chars[j])) {
                if chars[j].is_ascii_digit() {
                    digits += 1;
                }
                j += 1;
            }
            let prev_ok = i == 0 || !chars[i - 1].is_alphanumeric();
            if digits >= 10 && digits <= 13 && prev_ok && (chars[i] == '+' || chars[i] == '8' || chars[i] == '7' || chars[i] == '(') {
                // не съедаем хвостовой пробел
                let mut e = j;
                while e > i && chars[e - 1] == ' ' {
                    e -= 1;
                }
                res.push_str("[телефон]");
                i = e;
                continue;
            }
            res.extend(&chars[i..j.max(i + 1)]);
            i = j.max(i + 1);
            continue;
        }
        res.push(c);
        i += 1;
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    // только вымышленные ФИО
    #[test]
    fn replaces_and_restores() {
        let a = Anon::new(&["Лесовикова Марфа Ильинична".to_string(), "Громобоев Тихон".to_string()]);
        let t = "Файл: Лесовикова_МИ_вар3.docx\nВыполнила: Лесовикова Марфа Ильинична, гр. 31\nПроверил Громобоева работу? Нет, у Громобоева Т. другой вариант. Марфа — молодец. Почта marfa.l@example.org, тел. +7 (912) 345-67-89";
        let r = a.apply(t);
        assert!(!r.contains("Лесовиков"), "{}", r);
        assert!(!r.contains("Громобоев"), "{}", r);
        assert!(!r.contains("Ильинична"), "{}", r);
        assert!(r.contains("Выполнила: Студент 1, гр. 31"), "{}", r);
        assert!(r.contains("[почта]") && r.contains("[телефон]"), "{}", r);
        // имя без фамилии рядом не трогаем (это может быть обычное слово)
        assert!(r.contains("Марфа — молодец"), "{}", r);
        let back = a.restore("Студент 1 выполнил всё. Студенту 2 — проверить вариант.");
        assert_eq!(back, "Лесовикова Марфа Ильинична выполнил всё. Громобоев Тихон — проверить вариант.");
        // латиница в имени файла
        let r2 = a.apply("lesovikova_marfa_lab2.docx");
        assert!(!r2.to_lowercase().contains("lesovikova"), "{}", r2);
    }
    #[test]
    fn numbers_untouched() {
        assert_eq!(mask_contacts("Ответ: 1234567 и 2024 год, x = 3,14"), "Ответ: 1234567 и 2024 год, x = 3,14");
        let a = Anon::new(&["Студент 1".to_string()]);
        assert_eq!(a.restore("Студент 12"), "Студент 12");
    }
}
