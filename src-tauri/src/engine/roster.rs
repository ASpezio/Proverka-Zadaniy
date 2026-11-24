//! Список студентов из файла (Excel, Word, текст) или с фото: ФИО и, если указан, вариант.
//! Разбор устойчив к нумерации («1.», «№»), таблицам («ФИО | Вариант»),
//! строкам Excel («Строка 3 — A: 1 | B: Лесовиков Иван | C: 4») и подписям вроде «— вариант 3».

use super::text;

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub variant: Option<String>,
}

/// Слова, которые бывают в шапке списка и не являются ФИО.
const HEADER_WORDS: &[&str] = &[
    "фио", "ф.и.о.", "ф.и.о", "фамилия", "имя", "отчество", "список", "студент", "студенты", "студентов", "обучающиеся", "обучающийся", "группа", "группы", "вариант", "варианты", "номер", "итого", "всего",
    "преподаватель", "дисциплина", "курс", "специальность", "направление", "подпись", "оценка", "примечание", "дата", "семестр", "ведомость", "журнал", "посещаемость", "зачет", "зачёт", "экзамен",
];

/// Ячейка без служебного префикса столбца Excel («B: Лесовиков» → ("B", «Лесовиков»)).
fn split_col(cell: &str) -> (Option<String>, String) {
    let c = cell.trim();
    if let Some(p) = c.find(": ") {
        let head = &c[..p];
        if !head.is_empty() && head.len() <= 3 && head.chars().all(|ch| ch.is_ascii_uppercase()) {
            return (Some(head.to_string()), c[p + 2..].trim().to_string());
        }
    }
    (None, c.to_string())
}

/// Убирает нумерацию в начале: «12.», «12)», «№ 12», «12 »
fn strip_number(s: &str) -> (String, Option<String>) {
    let t = s.trim().trim_start_matches(['№', '#']).trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (s.trim().to_string(), None);
    }
    let rest = &t[digits.len()..];
    let rest2 = rest.trim_start_matches(['.', ')', ':', '-', '–', '—']);
    if rest2.len() == rest.len() && !rest.starts_with(char::is_whitespace) && !rest.is_empty() {
        return (s.trim().to_string(), None);
    }
    (rest2.trim().to_string(), Some(digits))
}

fn is_name_word(w: &str) -> bool {
    let w = w.trim_matches([',', ';']);
    let mut chars = w.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_alphabetic() || !first.is_uppercase() {
        return false;
    }
    let letters = w.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 1 && w.chars().all(|c| c.is_alphabetic() || matches!(c, '-' | '.' | '\'' | '’'))
}

/// Начало строки, похожее на ФИО: 2–4 слова с заглавной буквы (инициалы тоже подходят).
/// Возвращает (ФИО, остаток строки).
fn take_name(s: &str) -> Option<(String, String)> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut n = 0;
    while n < words.len() && n < 4 && is_name_word(words[n]) {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let name_words: Vec<&str> = words[..n].iter().map(|w| w.trim_matches([',', ';'])).collect();
    let low: Vec<String> = name_words.iter().map(|w| w.to_lowercase()).collect();
    if low.iter().any(|w| HEADER_WORDS.contains(&w.as_str())) {
        return None;
    }
    // одна фамилия допустима только если дальше нет текста (список «Лесовиков / Громобоев»)
    let full_words = name_words.iter().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2).count();
    if full_words == 0 || (n == 1 && words.len() > 1 && !words[1].chars().all(|c| c.is_ascii_digit() || "—–-.,;:()".contains(c))) {
        return None;
    }
    // фамилия должна быть словом, а не сокращением
    if name_words[0].chars().filter(|c| c.is_alphabetic()).count() < 2 {
        return None;
    }
    let name = name_words.join(" ");
    let rest = words[n..].join(" ");
    Some((name, rest))
}

/// Номер варианта в хвосте строки: «3», «— 3», «вариант 3», «3 вариант», «в. 3».
fn variant_in(rest: &str) -> Option<String> {
    let r = rest.trim().trim_start_matches(['—', '–', '-', ':', ',', ';', '(']).trim();
    if r.is_empty() {
        return None;
    }
    if let Some(v) = text::find_variant(r) {
        return Some(v);
    }
    // «в. 2», «в 2»
    let low = r.to_lowercase();
    for pre in ["в.", "в "] {
        if let Some(t) = low.strip_prefix(pre) {
            let d: String = t.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
            if !d.is_empty() && d.len() <= 3 {
                return Some(d.trim_start_matches('0').to_string()).filter(|s| !s.is_empty());
            }
        }
    }
    let first: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !first.is_empty() && first.len() <= 3 {
        let after = r[first.len()..].trim_start();
        if after.is_empty() || after.starts_with(['в', 'В', ')', ',', ';', '.']) || after.to_lowercase().starts_with("вар") {
            return Some(first.trim_start_matches('0').to_string()).filter(|s| !s.is_empty());
        }
    }
    None
}

/// Разбирает текст списка группы.
pub fn parse(src: &str) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    // столбец «Вариант» из шапки таблицы: буква Excel или порядковый номер ячейки
    let mut var_col: Option<(Option<String>, usize)> = None;
    for raw in src.lines() {
        let mut line = raw.trim().to_string();
        if line.is_empty() {
            continue;
        }
        // «Строка 3 — A: …» (так приложение читает Excel)
        if let Some(rest) = line.strip_prefix("Строка ") {
            if let Some(p) = rest.find(" — ") {
                if rest[..p].chars().all(|c| c.is_ascii_digit()) {
                    line = rest[p + " — ".len()..].to_string();
                }
            }
        }
        let cells: Vec<String> = if line.contains('|') {
            line.split('|').map(|s| s.to_string()).collect()
        } else if line.contains('\t') {
            line.split('\t').map(|s| s.to_string()).collect()
        } else if line.matches(';').count() >= 1 && !line.contains(' ') {
            line.split(';').map(|s| s.to_string()).collect()
        } else {
            vec![line.clone()]
        };
        let parsed: Vec<(Option<String>, String)> = cells.iter().map(|c| split_col(c)).filter(|(_, v)| !v.is_empty()).collect();
        // шапка с колонкой «Вариант»
        if let Some(i) = parsed.iter().position(|(_, v)| {
            let l = v.to_lowercase();
            (l.starts_with("вариант") || l == "вар." || l == "вар" || l == "№ варианта" || l == "номер варианта") && v.chars().filter(|c| c.is_ascii_digit()).count() == 0
        }) {
            if parsed.iter().all(|(_, v)| take_name(&strip_number(v).0).is_none()) {
                var_col = Some((parsed[i].0.clone(), i));
                continue;
            }
        }
        let mut found: Option<(usize, String, String)> = None;
        for (i, (_, v)) in parsed.iter().enumerate() {
            let (body, _) = strip_number(v);
            if let Some((name, rest)) = take_name(&body) {
                found = Some((i, name, rest));
                break;
            }
        }
        let (idx, name, rest) = match found {
            Some(x) => x,
            None => continue,
        };
        // вариант: из колонки «Вариант», из хвоста той же ячейки или из следующей ячейки-числа
        let mut variant = None;
        if let Some((col, pos)) = &var_col {
            let cell = match col {
                Some(c) => parsed.iter().find(|(k, _)| k.as_deref() == Some(c.as_str())).map(|(_, v)| v.clone()),
                None => parsed.get(*pos).map(|(_, v)| v.clone()),
            };
            variant = cell.and_then(|c| variant_in(&c));
        }
        if variant.is_none() {
            variant = variant_in(&rest);
        }
        if variant.is_none() && var_col.is_none() {
            // «ФИО | 3» — число после ФИО (число до ФИО — это номер по порядку)
            for (_, v) in parsed.iter().skip(idx + 1) {
                let t = v.trim();
                if !t.is_empty() && t.len() <= 3 && t.chars().all(|c| c.is_ascii_digit()) {
                    variant = Some(t.trim_start_matches('0').to_string()).filter(|s| !s.is_empty());
                    break;
                }
                if let Some(x) = text::find_variant(t) {
                    variant = Some(x);
                    break;
                }
            }
        }
        if out.iter().any(|r| r.name.to_lowercase() == name.to_lowercase()) {
            continue;
        }
        out.push(Row { name, variant });
    }
    out
}

/// Слова строки (буквы и цифры) в нижнем регистре, «ё» → «е».
pub fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase().replace('ё', "е").split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(|t| t.to_string()).collect()
}

/// Варианты записи латиницей: «Громобоева» → gromoboeva; «Ксения» → kseniya / ksenia.
pub fn translit(word: &str) -> Vec<String> {
    let mut outs: Vec<String> = vec![String::new(), String::new()];
    for c in word.to_lowercase().replace('ё', "е").chars() {
        let (a, b): (&str, &str) = match c {
            'а' => ("a", "a"), 'б' => ("b", "b"), 'в' => ("v", "v"), 'г' => ("g", "g"), 'д' => ("d", "d"), 'е' => ("e", "e"),
            'ж' => ("zh", "zh"), 'з' => ("z", "z"), 'и' => ("i", "i"), 'й' => ("y", "i"), 'к' => ("k", "k"), 'л' => ("l", "l"),
            'м' => ("m", "m"), 'н' => ("n", "n"), 'о' => ("o", "o"), 'п' => ("p", "p"), 'р' => ("r", "r"), 'с' => ("s", "s"),
            'т' => ("t", "t"), 'у' => ("u", "u"), 'ф' => ("f", "f"), 'х' => ("kh", "h"), 'ц' => ("ts", "c"), 'ч' => ("ch", "ch"),
            'ш' => ("sh", "sh"), 'щ' => ("shch", "sch"), 'ъ' | 'ь' => ("", ""), 'ы' => ("y", "y"), 'э' => ("e", "e"),
            'ю' => ("yu", "iu"), 'я' => ("ya", "ia"), '-' => ("-", "-"),
            x if x.is_ascii_alphanumeric() => {
                outs[0].push(x);
                outs[1].push(x);
                continue;
            }
            _ => continue,
        };
        outs[0].push_str(a);
        outs[1].push_str(b);
    }
    outs.dedup();
    outs
}

/// Упомянут ли студент `name` среди слов `hay`: по словам ФИО длиной от 4 букв,
/// которые есть только у него в списке (общие имена — «Дарья», «Софья» — не в счёт).
pub fn mentioned(name: &str, roster: &[String], hay: &[String]) -> bool {
    let words = tokens(name);
    words.iter().filter(|w| w.chars().count() >= 4 && !w.chars().all(|c| c.is_ascii_digit())).any(|w| {
        let unique = roster.iter().filter(|n| tokens(n).contains(w)).count() == 1;
        unique && (hay.contains(w) || translit(w).iter().any(|t| t.len() >= 4 && hay.contains(t)))
    })
}

/// Подсказка модели, распознающей фото списка.
pub const SYSTEM_VISION_ROSTER: &str = "Ты переписываешь список студентов с фотографии или скана. Каждого студента выведи отдельной строкой в формате: Фамилия Имя Отчество | вариант. Если варианта рядом нет — только ФИО. Порядок сохраняй как в списке. Заголовки, подписи, оценки и даты не переписывай. Ничего не добавляй от себя. Если имя неразборчиво, всё равно перепиши как видишь.";

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Row]) -> Vec<(String, Option<String>)> {
        v.iter().map(|r| (r.name.clone(), r.variant.clone())).collect()
    }

    #[test]
    fn plain_list() {
        let r = parse("Список группы 32\n1. Лесовиков Иван Иванович\n2) Громобоева Анна\n№3 Пряничков С.С.\n\nПреподаватель: Облачкова А.А.");
        assert_eq!(
            names(&r),
            vec![("Лесовиков Иван Иванович".into(), None), ("Громобоева Анна".into(), None), ("Пряничков С.С.".into(), None)]
        );
    }

    #[test]
    fn with_variants() {
        let r = parse("1. Лесовиков Иван — вариант 3\n2. Громобоева Анна - 5\nПряничков Семён 7 вариант\nТыквенников Кирилл (в. 2)");
        assert_eq!(
            names(&r),
            vec![
                ("Лесовиков Иван".into(), Some("3".into())),
                ("Громобоева Анна".into(), Some("5".into())),
                ("Пряничков Семён".into(), Some("7".into())),
                ("Тыквенников Кирилл".into(), Some("2".into()))
            ]
        );
    }

    #[test]
    fn excel_rows() {
        let r = parse("Лист «Лист1»\nСтрока 1 — A: № | B: ФИО | C: Вариант\nСтрока 2 — A: 1 | B: Лесовиков Иван | C: 4\nСтрока 3 — A: 2 | B: Громобоева Анна\nСтрока 4 — A: 3 | B: Пряничков Семён | C: 12");
        assert_eq!(
            names(&r),
            vec![("Лесовиков Иван".into(), Some("4".into())), ("Громобоева Анна".into(), None), ("Пряничков Семён".into(), Some("12".into()))]
        );
    }

    #[test]
    fn table_without_header() {
        let r = parse("1 | Лесовиков Иван Иванович | 3\n2 | Громобоева Анна Сергеевна | 1\nИТОГО | 2");
        assert_eq!(names(&r), vec![("Лесовиков Иван Иванович".into(), Some("3".into())), ("Громобоева Анна Сергеевна".into(), Some("1".into()))]);
    }

    #[test]
    fn pairs_in_names() {
        let roster: Vec<String> = ["Анна Ирискова", "Анна Снежинкова", "Ирина Лесовикова", "Мария Громобоева", "Мария Ветеркова"].iter().map(|s| s.to_string()).collect();
        let hay = tokens("Снежинкова и Ирискова 15 вариант.docx");
        let who: Vec<&String> = roster.iter().filter(|n| mentioned(n, &roster, &hay)).collect();
        assert_eq!(who.len(), 2);
        let hay = tokens("lesovikova_gromoboeva_32gr_V12.docx");
        let who: Vec<&String> = roster.iter().filter(|n| mentioned(n, &roster, &hay)).collect();
        assert_eq!(who, vec!["Ирина Лесовикова", "Мария Громобоева"]);
        // общее имя «Мария» само по себе никого не указывает
        assert!(!roster.iter().any(|n| mentioned(n, &roster, &tokens("Мария.docx"))));
    }

    #[test]
    fn caps_and_dupes() {
        let r = parse("ЛЕСОВИКОВ ИВАН\nЛесовиков Иван\nВариант 2\nГруппа 32гр");
        assert_eq!(names(&r), vec![("ЛЕСОВИКОВ ИВАН".into(), None)]);
    }
}
