//! Задания с фотографий и эталоны без файла ответов: нейросеть переписывает условие
//! с фото (любой предмет), при необходимости решает задания сама; перед генерацией
//! вариантов — задаёт вопросы и предлагает идеи.

use super::core::Core;
use super::json::{self, Json};
use super::text;
use std::path::PathBuf;
use std::sync::Arc;

pub const SYSTEM_VISION_TASK: &str = "Ты переписываешь с фотографий и сканов задание для учеников или студентов — по любому предмету: математика, информатика, физика, химия, русский язык и т. д. Перепиши ВСЁ дословно и по порядку: название и номер работы, тему, «Вариант N», номера заданий, условия, исходные данные, таблицы (строками с разделителем « | »), формулы в обычной записи (x^2, sqrt(x), a/b, ≤, ≥, π). Рисунки, графики и схемы опиши в квадратных скобках: [Рисунок: что изображено, подписи и числа]. Если на фото есть ответы, решения или ключ — перепиши их отдельно после строки «=== ОТВЕТЫ ===», с номерами вариантов и заданий. Если почерк или печать трудно разобрать — восстанови по смыслу и пометь догадку [?слово]. Ничего не решай и не добавляй от себя. Отвечай только переписанным текстом.";

pub const SYSTEM_SOLVE: &str = "Ты опытный преподаватель и составляешь эталонные ответы к заданию, чтобы по ним проверять работы. Реши каждое задание каждого варианта. Пиши кратко, но так, чтобы проверяющий понял, что считать верным: итоговый ответ, ключевые шаги или формулы, допустимые варианты формулировок. Если задание творческое или открытое — перечисли, что должно быть в хорошем ответе. Формат строго такой:\nВариант 1\n1. ответ\n2. ответ\nВариант 2\n1. ответ\n…\nЕсли вариантов нет — начни сразу с «1.». Если задание нельзя решить однозначно, напиши «(проверить вручную: почему)». Отвечай только ответами.";

pub const SYSTEM_PLAN: &str = "Ты опытный методист. Преподаватель хочет, чтобы нейросеть составила новые варианты задания по образцу. Прежде чем составлять, подумай, что в образце неоднозначно и о чём стоит спросить преподавателя, и что можно улучшить. Верни JSON: {\"questions\": [{\"q\": \"короткий вопрос\", \"options\": [\"вариант ответа 1\", \"вариант ответа 2\", \"вариант ответа 3\"]}], \"ideas\": [\"конкретная идея, как сделать варианты лучше\"]}. Вопросов — от 2 до 5, только по делу: уровень сложности, предметная область примеров, что менять в данных, формат ответа, нужны ли задания на конкретные программы и т. п. Первый вариант ответа в каждом вопросе — самый разумный по умолчанию. Идей — 3–5, коротко. Отвечай только JSON.";

/// Разделить распознанный текст на условие и ответы (если на фото были ответы).
pub fn split_answers(t: &str) -> (String, String) {
    for mark in ["=== ОТВЕТЫ ===", "=== Ответы ===", "===ОТВЕТЫ==="] {
        if let Some(i) = t.find(mark) {
            return (t[..i].trim().to_string(), t[i + mark.len()..].trim().to_string());
        }
    }
    (t.trim().to_string(), String::new())
}

/// Переписать задание (или ответы) с фотографий.
pub fn photo_text(core: &Arc<Core>, images: &[(String, String)], purpose: &str, subject: &str) -> Result<(String, String), String> {
    if images.is_empty() {
        return Err("Выберите фото задания".into());
    }
    let mut task = String::new();
    let mut key = String::new();
    for (i, img) in images.iter().enumerate() {
        (core.emit)("gen", Json::obj().with("done", i).with("total", images.len()).with("msg", format!("Нейросеть читает фото {} из {}", i + 1, images.len())).to_string());
        let ask = match purpose {
            "key" => format!("Это фото с ответами (ключом) к заданию{}. Перепиши ответы с номерами вариантов и заданий.", if subject.is_empty() { String::new() } else { format!(" по предмету «{}»", subject) }),
            _ => format!("Перепиши задание с этого фото (страница {} из {}){}.", i + 1, images.len(), if subject.is_empty() { String::new() } else { format!(", предмет — {}", subject) }),
        };
        let t = super::grader::vision_text(core, SYSTEM_VISION_TASK, &ask, std::slice::from_ref(img))?;
        let t = json::strip_think(&t).trim().to_string();
        if purpose == "key" {
            let (a, b) = split_answers(&t);
            key.push_str(&if b.is_empty() { a } else { b });
            key.push('\n');
        } else {
            let (a, b) = split_answers(&t);
            task.push_str(&a);
            task.push_str("\n\n");
            if !b.is_empty() {
                key.push_str(&b);
                key.push('\n');
            }
        }
    }
    Ok((task.trim().to_string(), key.trim().to_string()))
}

/// Эталонные ответы нейросетью.
pub fn solve(core: &Arc<Core>, task: &str, subject: &str, level: &str) -> Result<String, String> {
    if task.trim().is_empty() {
        return Err("Нет текста задания".into());
    }
    (core.emit)("gen", Json::obj().with("done", 0).with("total", 1).with("msg", "Нейросеть решает задания").to_string());
    let head = format!(
        "{}{}",
        if subject.is_empty() { String::new() } else { format!("Предмет: {}.\n", subject) },
        if level.is_empty() { String::new() } else { format!("Кто выполняет: {}. Ответы — на уровне этих учащихся.\n", level) }
    );
    let build = |max: usize| format!("{}# ЗАДАНИЕ\n{}\n\nСоставь эталонные ответы.", head, text::clip(task, max.saturating_sub(head.chars().count() + 200)));
    let t = super::grader::ask_once(core, SYSTEM_SOLVE, &build, false)?;
    Ok(t.trim().to_string())
}

/// Вопросы и идеи перед генерацией вариантов.
pub fn plan(core: &Arc<Core>, sample: &str, key: &str) -> Result<Json, String> {
    let build = |max: usize| format!("# ОБРАЗЕЦ ВАРИАНТА\n{}\n\n# ЭТАЛОН\n{}", text::clip(sample, max * 7 / 10), text::clip(key, max * 2 / 10));
    let t = super::grader::ask_once(core, SYSTEM_PLAN, &build, true)?;
    let j = json::extract_object(&t).ok_or("Модель вернула ответ не в формате JSON")?;
    let mut qs = Json::arr();
    for q in j.get("questions").as_arr().iter().take(6) {
        let qt = q.get("q").str_or("");
        let opts: Vec<String> = q.get("options").str_list().into_iter().filter(|o| !o.trim().is_empty()).take(5).collect();
        if !qt.trim().is_empty() {
            qs.push(Json::obj().with("q", qt).with("options", opts));
        }
    }
    let ideas: Vec<String> = j.get("ideas").str_list().into_iter().filter(|o| !o.trim().is_empty()).take(6).collect();
    Ok(Json::obj().with("questions", qs).with("ideas", ideas))
}

fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if "\\/:*?\"<>|\n\r\t".contains(c) { '_' } else { c }).collect();
    let t = t.trim().trim_matches('.').to_string();
    if t.is_empty() { "Задание".into() } else { t.chars().take(60).collect() }
}

/// Сохранить условие и ответы файлами в папке приложения и добавить их в задания.
/// Возвращает пути (условие, ответы).
pub fn save(core: &Core, code: &str, title: &str, scope: &str, task: &str, key: &str) -> Result<(Option<String>, Option<String>), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("Укажите шифр задания, например «ПР5» или «КР2»".into());
    }
    if task.trim().is_empty() && key.trim().is_empty() {
        return Err("Нет ни условия, ни ответов".into());
    }
    let dir: PathBuf = core.data_dir.join("tasks_text");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = super::core::now_s();
    let mut meta: super::core::TaskMeta = Default::default();
    let mut paths = Vec::new();
    let mut tp = None;
    let mut kp = None;
    if !task.trim().is_empty() {
        let p = dir.join(format!("{}_задание_{}.txt", safe_name(code), stamp));
        let body = format!("{}{}\n\n{}", code, if title.trim().is_empty() { String::new() } else { format!(". {}", title.trim()) }, task.trim());
        std::fs::write(&p, body).map_err(|e| format!("не удалось сохранить: {}", e))?;
        let s = p.to_string_lossy().to_string();
        meta.insert(s.clone(), ("task".into(), scope.trim().into(), code.into()));
        paths.push(s.clone());
        tp = Some(s);
    }
    if !key.trim().is_empty() {
        let p = dir.join(format!("{}_ответы_{}.txt", safe_name(code), stamp));
        std::fs::write(&p, format!("Ответы — {}\n\n{}", code, key.trim())).map_err(|e| format!("не удалось сохранить: {}", e))?;
        let s = p.to_string_lossy().to_string();
        meta.insert(s.clone(), ("key".into(), scope.trim().into(), code.into()));
        paths.push(s.clone());
        kp = Some(s);
    }
    let errs = core.add_tasks(&paths, Some(&meta), scope);
    if let Some(e) = errs.first() {
        return Err(e.clone());
    }
    Ok((tp, kp))
}

#[cfg(test)]
mod tests {
    #[test]
    fn split() {
        let (a, b) = super::split_answers("Вариант 1\n1. 2+2=?\n=== ОТВЕТЫ ===\n1. 4");
        assert_eq!(a, "Вариант 1\n1. 2+2=?");
        assert_eq!(b, "1. 4");
        assert_eq!(super::split_answers("только условие").1, "");
        assert_eq!(super::safe_name("ПР/5: «x»"), "ПР_5_ «x»");
    }
}
