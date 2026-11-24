//! Текстовые утилиты: кодировки, нормализация, сравнение.

use super::docx::{HID_END, HID_START};

/// CP1251 (кириллица Windows), байты 0x80..0xFF.
const CP1251: [u16; 128] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C, 0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0xFFFD, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407, 0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457, 0x0410, 0x0411, 0x0412, 0x0413, 0x0414, 0x0415, 0x0416, 0x0417, 0x0418, 0x0419, 0x041A, 0x041B, 0x041C, 0x041D, 0x041E, 0x041F, 0x0420, 0x0421, 0x0422, 0x0423, 0x0424, 0x0425, 0x0426, 0x0427, 0x0428, 0x0429, 0x042A, 0x042B, 0x042C, 0x042D, 0x042E, 0x042F, 0x0430, 0x0431, 0x0432, 0x0433, 0x0434, 0x0435, 0x0436, 0x0437, 0x0438, 0x0439, 0x043A, 0x043B, 0x043C, 0x043D, 0x043E, 0x043F, 0x0440, 0x0441, 0x0442, 0x0443, 0x0444, 0x0445, 0x0446, 0x0447, 0x0448, 0x0449, 0x044A, 0x044B, 0x044C, 0x044D, 0x044E, 0x044F,
];

pub fn decode_single_byte(b: u8, codepage: u32) -> char {
    if b < 0x80 {
        return b as char;
    }
    match codepage {
        1251 | 0 => char::from_u32(CP1251[(b - 0x80) as usize] as u32).unwrap_or('?'),
        _ => char::from_u32(b as u32).unwrap_or('?'), // latin-1 как запасной вариант
    }
}

/// Байты → строка: UTF-8 (с BOM или без), UTF-16 с BOM, иначе CP1251.
pub fn decode_bytes(b: &[u8]) -> String {
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> = b[2..]
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&x| decode_single_byte(x, 1251)).collect(),
    }
}

/// Приводит пробелы в порядок: убирает лишние пробелы и пустые строки.
pub fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let l: String = line.replace('\u{a0}', " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&l);
        out.push('\n');
    }
    out.trim().to_string()
}

/// То же, но сохраняет маркеры скрытого текста и склеивает соседние скрытые куски.
pub fn tidy_keep_markers(s: &str) -> String {
    let mut merged = s.to_string();
    let pair = format!("{}{}", HID_END, HID_START);
    while merged.contains(&pair) {
        merged = merged.replace(&pair, "");
    }
    // пустые скрытые фрагменты
    let empty = format!("{}{}", HID_START, HID_END);
    merged = merged.replace(&empty, "");
    tidy(&merged)
}

/// Нормализация для поиска совпадений: нижний регистр, ё→е, только буквы/цифры.
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        let c = match c {
            'ё' | 'Ё' => 'е',
            _ => c,
        };
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

pub fn words(s: &str) -> Vec<String> {
    norm(s).split(' ').filter(|w| !w.is_empty()).map(|w| w.to_string()).collect()
}

/// Ищет нормализованную фразу как последовательность целых слов.
pub fn contains_phrase(norm_haystack: &str, phrase: &str) -> bool {
    let p = norm(phrase);
    if p.is_empty() {
        return false;
    }
    let h = format!(" {} ", norm_haystack);
    h.contains(&format!(" {} ", p))
}

/// Обрезка строки по символам с многоточием.
pub fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n…[текст обрезан]");
    out
}

/// Режимы сжатия длинной работы (что делать, если она не помещается в контекст):
/// head — обрезать конец; headtail — оставить начало и конец; dense — убрать лишние
/// пробелы, пустые и повторяющиеся строки, потом обрезать; spread — равные куски
/// по всей работе (каждое задание попадёт хотя бы началом).
pub const COMPRESS_MODES: &[&str] = &["dense", "head", "headtail", "spread", "summary"];

pub fn compress(s: &str, max_chars: usize, mode: &str) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    match mode {
        "head" => clip(s, max_chars),
        "headtail" => head_tail(s, max_chars),
        "spread" => spread(&densify(s), max_chars),
        // dense и summary (если пересказ не помог или недоступен)
        _ => {
            let d = densify(s);
            if d.chars().count() <= max_chars {
                d
            } else {
                head_tail(&d, max_chars)
            }
        }
    }
}

/// Без пустого места: пробелы, пустые строки, повторяющиеся строки, линии из «-—=_.».
pub fn densify(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in s.lines() {
        let l: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            continue;
        }
        if l.chars().count() >= 3 && l.chars().all(|c| "-—=_.*•·~#|+ ".contains(c)) {
            continue;
        }
        // длинные повторяющиеся строки (колонтитулы, копии условия) — один раз;
        // заголовки файлов «=== Файл …» и короткие ответы не трогаем
        if l.chars().count() >= 25 && !l.starts_with("=== Файл") {
            let key = l.to_lowercase();
            if !seen.insert(key) {
                continue;
            }
        }
        out.push(l);
    }
    out.join("\n")
}

fn head_tail(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let mark_len = 60;
    let room = max_chars.saturating_sub(mark_len).max(200);
    let head = room * 65 / 100;
    let tail = room - head;
    let skipped = chars.len().saturating_sub(head + tail);
    format!(
        "{}\n…[пропущено {} символов из середины работы]…\n{}",
        chars[..head.min(chars.len())].iter().collect::<String>(),
        skipped,
        chars[chars.len().saturating_sub(tail)..].iter().collect::<String>()
    )
}

fn spread(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let pieces = (max_chars / 1200).clamp(3, 12);
    let mark = "\n…[пропуск]…\n";
    let per = (max_chars.saturating_sub(pieces * mark.chars().count())) / pieces;
    let step = chars.len() / pieces;
    let mut out = String::new();
    for k in 0..pieces {
        let st = k * step;
        // начинаем с начала строки, если она рядом
        let st = (st..(st + 200).min(chars.len())).find(|&i| i == 0 || chars[i - 1] == '\n').unwrap_or(st);
        let en = (st + per).min(chars.len());
        if k > 0 {
            out.push_str(mark);
        }
        out.extend(&chars[st..en]);
    }
    out
}

/// Обрезка работы из нескольких файлов: каждый файл («=== Файл «…» ===»)
/// получает свою долю, чтобы второй файл (например, Excel) не отрезался целиком.
pub fn clip_parts(s: &str, max_chars: usize) -> String {
    compress_parts(s, max_chars, "head")
}

/// Как `clip_parts`, но каждый файл сжимается выбранным режимом.
pub fn compress_parts(s: &str, max_chars: usize, mode: &str) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let sep = "\n\n=== Файл «";
    let mut parts: Vec<String> = Vec::new();
    let mut rest = s;
    loop {
        // ищем заголовок следующего файла, не считая начала текущей части
        let skip = rest.char_indices().nth(1).map(|(i, _)| i).unwrap_or(rest.len());
        let Some(i) = rest[skip..].find(sep).map(|i| i + skip) else { break };
        parts.push(rest[..i].to_string());
        rest = &rest[i..];
    }
    parts.push(rest.to_string());
    if parts.len() < 2 {
        return compress(s, max_chars, mode);
    }
    // поровну, но короткий файл отдаёт неиспользованное остальным
    let n = parts.len();
    let mut budget: Vec<usize> = vec![0; n];
    let mut left = max_chars;
    let mut open: Vec<usize> = (0..n).collect();
    loop {
        if open.is_empty() {
            break;
        }
        let share = left / open.len();
        let small: Vec<usize> = open.iter().copied().filter(|&i| parts[i].chars().count() <= share).collect();
        if small.is_empty() {
            for &i in &open {
                budget[i] = share;
            }
            break;
        }
        for i in small {
            budget[i] = parts[i].chars().count();
            left -= budget[i];
            open.retain(|&x| x != i);
        }
    }
    parts.iter().zip(budget).map(|(p, b)| compress(p, b.max(300), mode)).collect::<Vec<_>>().join("")
}

/// Короткий фрагмент вокруг найденной фразы (для доказательств в ведомости).
pub fn snippet_around(text: &str, phrase: &str, radius: usize) -> Option<String> {
    let lt = text.to_lowercase().replace('ё', "е");
    let lp = phrase.to_lowercase().replace('ё', "е");
    let lp = lp.trim();
    if lp.is_empty() {
        return None;
    }
    let byte = lt.find(lp)?;
    // переводим позицию в символы
    let ci = lt[..byte].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let ci = ci.min(chars.len());
    let st = ci.saturating_sub(radius);
    let en = (ci + lp.chars().count() + radius).min(chars.len()).max(st);
    let mut s: String = chars[st..en].iter().collect();
    s = s.replace('\n', " ");
    Some(format!("{}{}{}", if st > 0 { "…" } else { "" }, s.trim(), if en < chars.len() { "…" } else { "" }))
}

/// Номер варианта из строки: «Вариант 12», «Вариант №12», «В12», «вар. 12», «В-12».
pub fn find_variant(s: &str) -> Option<String> {
    let low: Vec<char> = s.to_lowercase().replace('ё', "е").chars().collect();
    let n = low.len();
    let digits_at = |mut i: usize| -> Option<(String, usize)> {
        while i < n && matches!(low[i], ' ' | '№' | '#' | ':' | '.' | '-' | '_' | '\u{a0}') {
            i += 1;
        }
        let st = i;
        while i < n && low[i].is_ascii_digit() {
            i += 1;
        }
        if i > st && i - st <= 3 {
            Some((low[st..i].iter().collect(), i))
        } else {
            None
        }
    };
    // «вариант N» / «вар. N» / «variant N»
    for key in ["вариант", "варинт", "вариан", "вараинт", "вар.", "вар ", "variant"] {
        let k: Vec<char> = key.chars().collect();
        let mut i = 0;
        while i + k.len() <= n {
            if low[i..i + k.len()] == k[..] {
                if let Some((d, _)) = digits_at(i + k.len()) {
                    return Some(trim_zeros(&d));
                }
            }
            i += 1;
        }
    }
    // «В12» как отдельное слово (кириллическая или латинская B)
    let mut i = 0;
    while i < n {
        // ссылки на ячейки в формулах («=B2*C2», «СУММ(B2:B9)», «A1-B2») — не вариант
        let in_formula = i > 0 && ("=(:;$!*+/<>&^,".contains(low[i - 1]) || (low[i - 1] == '-' && i > 1 && (low[i - 2].is_alphanumeric() || low[i - 2] == ')')));
        if (low[i] == 'в' || low[i] == 'v' || low[i] == 'b') && (i == 0 || !low[i - 1].is_alphanumeric()) && !in_formula {
            let mut j = i + 1;
            if j < n && (low[j] == '-' || low[j] == '_') {
                j += 1;
            }
            let st = j;
            while j < n && low[j].is_ascii_digit() {
                j += 1;
            }
            if j > st && j - st <= 2 && (j == n || !low[j].is_alphabetic() || low[j] == 'в' || low[j] == 'д') {
                // «В20», «В9В» — буква после номера допускается для подгрупп
                if j == n || !low[j].is_alphanumeric() || (j + 1 >= n || !low[j + 1].is_alphanumeric()) {
                    return Some(trim_zeros(&low[st..j].iter().collect::<String>()));
                }
            }
        }
        i += 1;
    }
    None
}

/// Вариант, написанный словом: «Вариант 3», «вариант №3», «3 вариант». Без коротких
/// форм вроде «В3» — их в тексте легко спутать со ссылкой на ячейку или предлогом.
pub fn find_variant_word(s: &str) -> Option<String> {
    let low: Vec<char> = s.to_lowercase().replace('ё', "е").chars().collect();
    let n = low.len();
    let key: Vec<char> = "вариант".chars().collect();
    let mut after: Option<String> = None;
    let mut i = 0;
    while i + key.len() <= n {
        if low[i..i + key.len()] == key[..] && (i == 0 || !low[i - 1].is_alphabetic()) {
            // «вариант 3»
            let mut j = i + key.len();
            while j < n && matches!(low[j], ' ' | '№' | '#' | ':' | '.' | '-' | '_' | '\u{a0}') {
                j += 1;
            }
            let st = j;
            while j < n && low[j].is_ascii_digit() {
                j += 1;
            }
            if j > st && j - st <= 3 && (j == n || !low[j].is_ascii_digit()) {
                return Some(trim_zeros(&low[st..j].iter().collect::<String>()));
            }
            // «3 вариант»
            if after.is_none() {
                let mut k = i;
                while k > 0 && matches!(low[k - 1], ' ' | '_' | '-' | '\u{a0}') {
                    k -= 1;
                }
                let en = k;
                while k > 0 && low[k - 1].is_ascii_digit() {
                    k -= 1;
                }
                if en > k && en - k <= 2 && (k == 0 || !low[k - 1].is_alphanumeric()) {
                    after = Some(trim_zeros(&low[k..en].iter().collect::<String>()));
                }
            }
        }
        i += 1;
    }
    after
}

/// Вариант из имени файла. Кроме форм, которые понимает [`find_variant`], в именах
/// файлов встречаются «Колокольчикова 14В», «Пескарева_11 В», «27ВАР», «9 вариант», «ВР11», «Вр16».
pub fn find_variant_in_name(name: &str) -> Option<String> {
    let stem = match name.rfind('.') {
        Some(p) if name.len() - p <= 5 => &name[..p],
        _ => name,
    };
    if let Some(v) = find_variant_word(stem).or_else(|| find_variant(stem)) {
        return Some(v);
    }
    let low: Vec<char> = stem.to_lowercase().replace('ё', "е").chars().collect();
    let n = low.len();
    let mut i = 0;
    while i < n {
        // «ВР11», «Вр16», «var12»
        for key in ["вр", "var"] {
            let k: Vec<char> = key.chars().collect();
            if i + k.len() <= n && low[i..i + k.len()] == k[..] && (i == 0 || !low[i - 1].is_alphabetic()) {
                let mut j = i + k.len();
                while j < n && matches!(low[j], ' ' | '.' | '-' | '_' | '№') {
                    j += 1;
                }
                let st = j;
                while j < n && low[j].is_ascii_digit() {
                    j += 1;
                }
                if j > st && j - st <= 3 && (j == n || !low[j].is_alphanumeric()) {
                    return Some(trim_zeros(&low[st..j].iter().collect::<String>()));
                }
            }
        }
        // «14В», «11 В», «27ВАР», «5v»
        if low[i].is_ascii_digit() && (i == 0 || !low[i - 1].is_alphanumeric()) {
            let st = i;
            let mut j = i;
            while j < n && low[j].is_ascii_digit() {
                j += 1;
            }
            let en = j;
            while j < n && matches!(low[j], ' ' | '_' | '-') {
                j += 1;
            }
            if en - st <= 2 && j < n && (low[j] == 'в' || low[j] == 'v') {
                let rest: String = low[j + 1..].iter().collect();
                let ok = j + 1 == n || !low[j + 1].is_alphanumeric() || rest.starts_with("ар") || rest.starts_with("ar");
                if ok {
                    return Some(trim_zeros(&low[st..en].iter().collect::<String>()));
                }
            }
            i = en;
            continue;
        }
        i += 1;
    }
    None
}

fn trim_zeros(s: &str) -> String {
    let t = s.trim_start_matches('0');
    if t.is_empty() {
        "0".into()
    } else {
        t.to_string()
    }
}

/// Код работы вида «ПР2», «ПР 2», «PR2», «Практическая работа № 2».
pub fn find_work_code(s: &str) -> Option<String> {
    let low: Vec<char> = s.to_lowercase().chars().collect();
    let n = low.len();
    let pats: [&str; 4] = ["практическая работа", "пр", "pr", "лр"];
    for p in pats {
        let k: Vec<char> = p.chars().collect();
        let mut i = 0;
        while i + k.len() <= n {
            if low[i..i + k.len()] == k[..] && (i == 0 || !low[i - 1].is_alphabetic()) {
                let mut j = i + k.len();
                while j < n && matches!(low[j], ' ' | '№' | '_' | '-' | '.' | '\u{a0}') {
                    j += 1;
                }
                let st = j;
                while j < n && low[j].is_ascii_digit() {
                    j += 1;
                }
                if j > st && j - st <= 2 {
                    let num: String = low[st..j].iter().collect();
                    let prefix = if p == "лр" { "ЛР" } else { "ПР" };
                    return Some(format!("{}{}", prefix, trim_zeros(&num)));
                }
            }
            i += 1;
        }
    }
    None
}

/// Простое «похоже на» по словам: доля слов b, встречающихся в a.
pub fn word_overlap(a: &str, b: &str) -> f64 {
    let wa: std::collections::HashSet<String> = words(a).into_iter().filter(|w| w.chars().count() > 3).collect();
    let wb: Vec<String> = words(b).into_iter().filter(|w| w.chars().count() > 3).collect();
    if wb.is_empty() {
        return 0.0;
    }
    wb.iter().filter(|w| wa.contains(*w)).count() as f64 / wb.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variants() {
        assert_eq!(find_variant("Вариант 20.docx").as_deref(), Some("20"));
        assert_eq!(find_variant("Задание 1. В20.").as_deref(), Some("20"));
        assert_eq!(find_variant("ПР2_Лесовиков_В3").as_deref(), Some("3"));
        assert_eq!(find_variant("Вариант №07").as_deref(), Some("7"));
        assert_eq!(find_variant("Вариант 9В.docx").as_deref(), Some("9"));
        assert_eq!(find_variant("Выполнил Громобоев").as_deref(), None);
        assert_eq!(find_variant("Строка 2 — A: Яблоки | D: =B2*C2 → 150 | E: =СУММ(B2:B9)-B1").as_deref(), None);
        assert_eq!(find_work_code("ПР2_Программное_обеспечение").as_deref(), Some("ПР2"));
        assert_eq!(find_work_code("Практическая работа № 2").as_deref(), Some("ПР2"));
        assert_eq!(find_work_code("Проверка").as_deref(), None);
    }
    #[test]
    fn phrase() {
        let h = norm("Аптека «Медуница» — лучшая! Согласно  методике, вывод.");
        assert!(contains_phrase(&h, "медуница"));
        assert!(contains_phrase(&h, "Согласно методике"));
        assert!(!contains_phrase(&h, "медун"));
    }
}

#[cfg(test)]
mod clip_tests {
    #[test]
    fn parts() {
        let a = "А".repeat(5000);
        let b = format!("\n\n=== Файл «x.xlsx» ===\n{}", "Б".repeat(800));
        let r = super::clip_parts(&format!("{}{}", a, b), 2000);
        assert!(r.contains("=== Файл «x.xlsx»"));
        assert!(r.matches('Б').count() >= 750);
    }
}

#[cfg(test)]
mod variant_names {
    use super::*;
    #[test]
    fn names() {
        let cases = [
            ("задания 1 , 3 Ветерков Пётр 31 гр Вариант_9 код", Some("9")),
            ("lesovikova_gromoboeva_32gr_V12.docx", Some("12")),
            ("veterkova_kryzhovnikova_variant_19.docx", Some("19")),
            ("Медовиков_В3.docx", Some("3")),
            ("Ромашкова 9 вариант.docx", Some("9")),
            ("Пескарева Анна_11 В.docx", Some("11")),
            ("Колокольчикова 14В.docx", Some("14")),
            ("Ирискова и Снежинкова 15 вариант.docx", Some("15")),
            ("Пирожкова, В25.docx", Some("25")),
            ("СКАЗОЧКИНА И ПРЯЖКИНА 27ВАР.docx", Some("27")),
            ("«ПР1_Вареничкова_В14».docx", Some("14")),
            ("Брусникова Мария 3 вариант..docx", Some("3")),
            ("Документ Microsoft Word ПР_ЛЕДЕНЦОВА_B13.docx", Some("13")),
            ("ПР1_Пастилов_ВР11.docx", Some("11")),
            ("ПР1_Облачкова_Вр16.docx", Some("16")),
            ("Работа №1 Наковальникова Вариант 9.docx", Some("9")),
            ("ПР_Крыжовникова_В6.dock.docx", Some("6")),
            ("Брусникова Пр2 вариант3 (1).pdf", Some("3")),
            ("задание 1.docx", None),
            ("ПР2_Лесовиков.docx", None),
            ("Конспект.docx", None),
            ("31 гр Громобоев.docx", None),
        ];
        for (n, v) in cases {
            assert_eq!(find_variant_in_name(n).as_deref(), v, "{}", n);
        }
        assert_eq!(find_variant_word("Выполнил Громобоев, 9 вариант").as_deref(), Some("9"));
        assert_eq!(find_variant_word("ячейка B5 и 5 в степени").as_deref(), None);
        assert_eq!(find_variant_word("Вариант №07").as_deref(), Some("7"));
    }

    #[test]
    fn compress_modes() {
        let body: String = (1..=40).map(|i| format!("Задание {}. Ответ: {}\n\n\n   \n----------\nКолонтитул: Лабораторная работа по информатике\n", i, i * 7)).collect();
        let d = super::densify(&body);
        assert_eq!(d.matches("Колонтитул").count(), 1);
        assert!(!d.contains("-----"));
        let ht = super::compress(&body, 400, "headtail");
        assert!(ht.contains("Задание 1.") && ht.contains("Задание 40.") && ht.contains("пропущено"));
        assert!(ht.chars().count() <= 400);
        let sp = super::compress(&body, 3600, "spread");
        assert!(sp.contains("Задание 1.") && sp.contains("Задание 2") && sp.contains("Задание 3"));
        assert!(sp.chars().count() <= 3600, "{}", sp.chars().count());
        let dn = super::compress(&body, 900, "dense");
        assert!(dn.contains("Задание 40."), "{}", dn);
    }
}
