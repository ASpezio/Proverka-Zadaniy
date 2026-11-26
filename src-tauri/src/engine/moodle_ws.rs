//! Прямое подключение к Moodle через веб-службы (REST, ключ доступа — token).
//!
//! Что нужно на стороне Moodle: включённые веб-службы и протокол REST. У большинства
//! сайтов уже включена служба мобильного приложения (moodle_mobile_app) — тогда ключ
//! можно получить логином и паролем (`login/token.php`), пароль приложение не хранит.
//! Иначе ключ создаётся в профиле: «Настройки → Ключи безопасности».
//!
//! Функции Moodle, которыми пользуется приложение:
//! * core_webservice_get_site_info — проверка ключа, имя сайта и пользователя;
//! * core_enrol_get_users_courses — курсы преподавателя;
//! * core_enrol_get_enrolled_users, core_group_get_course_groups — студенты и группы курса;
//! * mod_assign_get_assignments, mod_assign_get_submissions — задания и ответы (файлы скачиваются
//!   через webservice/pluginfile.php с тем же ключом);
//! * gradereport_user_get_grade_items — оценки и отзывы всего курса (задания и тесты);
//! * mod_assign_save_grade — оценка и отзыв в задание.

use super::http::HttpClient;
use super::json::{self, Json};

pub struct Ws {
    pub base: String,
    pub token: String,
}

/// Кодирование для application/x-www-form-urlencoded и строки запроса.
pub fn urlenc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b' ' => o.push('+'),
            _ => o.push_str(&format!("%{:02X}", b)),
        }
    }
    o
}

/// «moodle.college.ru/», «https://moodle.college.ru/course/view.php?id=5» → «https://moodle.college.ru».
pub fn normalize_base(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if !u.starts_with("http://") && !u.starts_with("https://") {
        u = format!("https://{}", u);
    }
    for cut in ["/login/", "/course/", "/mod/", "/my/", "/user/", "/admin/", "/grade/", "/webservice/", "/index.php", "/?"] {
        if let Some(i) = u.find(cut) {
            if i > 8 {
                u.truncate(i);
            }
        }
    }
    u.trim_end_matches('/').to_string()
}

fn moodle_error(j: &Json) -> Option<String> {
    if j.get("exception").is_null() && j.get("errorcode").is_null() && j.get("error").is_null() {
        return None;
    }
    let code = j.get("errorcode").str_or("");
    let msg = j.get("message").str_or(&j.get("error").str_or(""));
    let hint = match code.as_str() {
        "invalidtoken" => " — ключ неверный или устарел: получите новый",
        "accessexception" | "nopermissions" | "webservicesnotenabled" => " — у ключа нет доступа к этой функции: попросите администратора Moodle включить её для службы или используйте службу мобильного приложения",
        "servicenotavailable" | "enablewsdescription" => " — веб-службы на сайте выключены: их включает администратор Moodle",
        _ => "",
    };
    Some(format!("Moodle: {}{}{}", msg, if code.is_empty() { String::new() } else { format!(" ({})", code) }, hint))
}

/// Ключ по логину и паролю (служба мобильного приложения). Пароль никуда не сохраняется.
pub fn get_token(http: &dyn HttpClient, base: &str, user: &str, pass: &str, service: &str) -> Result<String, String> {
    let url = format!("{}/login/token.php", normalize_base(base));
    let body = format!("username={}&password={}&service={}", urlenc(user), urlenc(pass), urlenc(if service.is_empty() { "moodle_mobile_app" } else { service }));
    let h = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string()), ("User-Agent".to_string(), "proverka-zadaniy".to_string())];
    let r = http.request("POST", &url, &h, Some(&body), 30).map_err(|e| format!("Не удалось связаться с Moodle: {}", e))?;
    let j = json::parse(&r.body).map_err(|_| format!("Moodle ответил не так, как ожидалось (код {}). Проверьте адрес сайта.", r.status))?;
    if let Some(e) = moodle_error(&j) {
        return Err(e);
    }
    let t = j.get("token").str_or("");
    if t.is_empty() {
        return Err("Moodle не выдал ключ: возможно, служба мобильного приложения выключена — создайте ключ в профиле («Настройки → Ключи безопасности»)".into());
    }
    Ok(t)
}

impl Ws {
    pub fn new(base: &str, token: &str) -> Ws {
        Ws { base: normalize_base(base), token: token.trim().to_string() }
    }

    /// Вызов функции веб-службы: параметры — пары «ключ[0][поле]» → значение.
    pub fn call(&self, http: &dyn HttpClient, func: &str, params: &[(String, String)]) -> Result<Json, String> {
        if self.base.is_empty() || self.token.is_empty() {
            return Err("Подключение к Moodle не настроено: укажите адрес сайта и ключ".into());
        }
        let url = format!("{}/webservice/rest/server.php?moodlewsrestformat=json", self.base);
        let mut body = format!("wstoken={}&wsfunction={}", urlenc(&self.token), urlenc(func));
        for (k, v) in params {
            body.push('&');
            body.push_str(&urlenc(k));
            body.push('=');
            body.push_str(&urlenc(v));
        }
        let h = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string()), ("User-Agent".to_string(), "proverka-zadaniy".to_string())];
        let r = http.request("POST", &url, &h, Some(&body), 90).map_err(|e| format!("Не удалось связаться с Moodle: {}", e))?;
        if r.status == 404 {
            return Err("Moodle: адрес веб-служб не найден — проверьте адрес сайта".into());
        }
        let j = json::parse(&r.body).map_err(|_| format!("Moodle ответил не JSON (код {}): веб-службы или протокол REST выключены", r.status))?;
        if let Some(e) = moodle_error(&j) {
            return Err(e);
        }
        Ok(j)
    }

    pub fn site_info(&self, http: &dyn HttpClient) -> Result<Json, String> {
        self.call(http, "core_webservice_get_site_info", &[])
    }

    /// Курсы пользователя: [{id, fullname, shortname}]
    pub fn courses(&self, http: &dyn HttpClient, userid: i64) -> Result<Vec<Json>, String> {
        let j = self.call(http, "core_enrol_get_users_courses", &[("userid".into(), userid.to_string())])?;
        Ok(j.as_arr().to_vec())
    }

    /// Задания курса: [{id, cmid, name, duedate, grade, intro}]
    pub fn assignments(&self, http: &dyn HttpClient, courseid: i64) -> Result<Vec<Json>, String> {
        let j = self.call(http, "mod_assign_get_assignments", &[("courseids[0]".into(), courseid.to_string())])?;
        Ok(j.get("courses").as_arr().iter().flat_map(|c| c.get("assignments").as_arr().to_vec()).collect())
    }

    /// Студенты курса: [{id, fullname, firstname, lastname, email, groups:[{id,name}]}]
    pub fn users(&self, http: &dyn HttpClient, courseid: i64) -> Result<Vec<Json>, String> {
        let j = self.call(http, "core_enrol_get_enrolled_users", &[("courseid".into(), courseid.to_string())])?;
        Ok(j.as_arr().to_vec())
    }

    /// Группы курса: [{id, name}]
    pub fn groups(&self, http: &dyn HttpClient, courseid: i64) -> Result<Vec<Json>, String> {
        let j = self.call(http, "core_group_get_course_groups", &[("courseid".into(), courseid.to_string())])?;
        Ok(j.as_arr().to_vec())
    }

    /// Ответы задания (последняя попытка каждого студента).
    pub fn submissions(&self, http: &dyn HttpClient, assignid: i64) -> Result<Vec<Submission>, String> {
        let j = self.call(http, "mod_assign_get_submissions", &[("assignmentids[0]".into(), assignid.to_string())])?;
        let mut out = Vec::new();
        for a in j.get("assignments").as_arr() {
            for s in a.get("submissions").as_arr() {
                let mut files = Vec::new();
                let mut text = String::new();
                for pl in s.get("plugins").as_arr() {
                    for fa in pl.get("fileareas").as_arr() {
                        for f in fa.get("files").as_arr() {
                            let url = f.get("fileurl").str_or("");
                            let name = f.get("filename").str_or("");
                            if !url.is_empty() && !name.is_empty() && name != "." {
                                files.push(MFile { name, url, size: f.get("filesize").f64_or(0.0) as u64, mtime: f.get("timemodified").f64_or(0.0) as u64 });
                            }
                        }
                    }
                    for ef in pl.get("editorfields").as_arr() {
                        let t = ef.get("text").str_or("");
                        if !t.trim().is_empty() {
                            text.push_str(&t);
                        }
                    }
                }
                out.push(Submission {
                    userid: s.get("userid").f64_or(0.0) as i64,
                    status: s.get("status").str_or(""),
                    modified: s.get("timemodified").f64_or(0.0) as u64,
                    files,
                    text,
                });
            }
        }
        Ok(out)
    }

    /// Скачать файл ответа (ссылка webservice/pluginfile.php + ключ).
    pub fn download(&self, http: &dyn HttpClient, url: &str) -> Result<Vec<u8>, String> {
        let u = if url.contains('?') { format!("{}&token={}", url, urlenc(&self.token)) } else { format!("{}?token={}", url, urlenc(&self.token)) };
        let u = u.replace("/pluginfile.php", if url.contains("/webservice/pluginfile.php") { "/pluginfile.php" } else { "/webservice/pluginfile.php" });
        let r = http.get_raw(&u, &[], 200 * 1024 * 1024, 120)?;
        if r.status >= 400 {
            return Err(format!("Moodle не отдал файл (код {})", r.status));
        }
        // вместо файла пришла ошибка веб-службы
        if r.body.first() == Some(&b'{') {
            if let Ok(j) = json::parse(&String::from_utf8_lossy(&r.body)) {
                if let Some(e) = moodle_error(&j) {
                    return Err(e);
                }
            }
        }
        Ok(r.body)
    }

    /// Оценки и отзывы курса по всем элементам: строки как у экспорта журнала оценок
    /// («Задание:… (Значение)», «… (Отзыв)»), чтобы их разобрал тот же импорт.
    pub fn grade_rows(&self, http: &dyn HttpClient, courseid: i64, only_users: Option<&[i64]>) -> Result<(Vec<Vec<String>>, Vec<(String, f64)>), String> {
        let j = self.call(http, "gradereport_user_get_grade_items", &[("courseid".into(), courseid.to_string())])?;
        let mut heads: Vec<String> = vec!["Имя".into(), "Фамилия".into(), "Адрес электронной почты".into()];
        let mut cols: Vec<(i64, String)> = Vec::new(); // (id элемента, заголовок без вида)
        let mut rows: Vec<Vec<String>> = Vec::new();
        let users = j.get("usergrades").as_arr();
        // столбцы — по элементам первого студента (у всех одинаковые)
        let prefix = |m: &str, t: &str| -> &'static str {
            match (m, t) {
                ("quiz", _) => "Тест:",
                ("assign", _) => "Задание:",
                ("lesson", _) => "Лекция:",
                ("workshop", _) => "Семинар:",
                ("forum", _) => "Форум:",
                (_, "course") => "",
                (_, "manual") => "Ручная оценка:",
                _ => "Элемент курса:",
            }
        };
        for u in users {
            for it in u.get("gradeitems").as_arr() {
                let id = it.get("id").f64_or(0.0) as i64;
                let typ = it.get("itemtype").str_or("");
                if typ == "category" || cols.iter().any(|c| c.0 == id) {
                    continue;
                }
                let name = if typ == "course" { "Итоговая оценка за курс".to_string() } else { format!("{}{}", prefix(&it.get("itemmodule").str_or(""), &typ), it.get("itemname").str_or("")) };
                cols.push((id, name));
            }
        }
        // шкала каждого элемента (максимум в Moodle) — для выгрузки оценок обратно
        let mut scales: Vec<(String, f64)> = Vec::new();
        for (id, n) in &cols {
            let max = users.iter().flat_map(|u| u.get("gradeitems").as_arr().to_vec()).find(|x| x.get("id").f64_or(0.0) as i64 == *id).map(|x| x.get("grademax").f64_or(0.0)).unwrap_or(0.0);
            scales.push((n.clone(), max));
        }
        for (_, n) in &cols {
            heads.push(format!("{} (Значение)", n));
            heads.push(format!("{} (Отзыв)", n));
        }
        rows.push(heads);
        for u in users {
            let uid = u.get("userid").f64_or(0.0) as i64;
            if let Some(only) = only_users {
                if !only.contains(&uid) {
                    continue;
                }
            }
            let full = u.get("userfullname").str_or("");
            let mut r = vec![String::new(), full, String::new()];
            for (id, _) in &cols {
                let it = u.get("gradeitems").as_arr().iter().find(|x| x.get("id").f64_or(0.0) as i64 == *id).cloned().unwrap_or(Json::Null);
                // оценка в шкале 5: из «graderaw» и максимума элемента
                let raw = it.get("graderaw").as_f64();
                let max = it.get("grademax").f64_or(0.0);
                let val = match raw {
                    Some(v) if max > 0.0 => {
                        let five = if (max - 5.0).abs() < 1e-9 { v } else { v / max * 5.0 };
                        super::core::fmt_grade((five * 100.0).round() / 100.0).replace(',', ".")
                    }
                    _ => "-".into(),
                };
                r.push(val);
                r.push(it.get("feedback").str_or(""));
            }
            rows.push(r);
        }
        Ok((rows, scales))
    }

    /// Оценка и отзыв в задание. `grade` — уже в шкале задания.
    pub fn save_grade(&self, http: &dyn HttpClient, assignid: i64, userid: i64, grade: f64, feedback: Option<&str>) -> Result<(), String> {
        let mut p: Vec<(String, String)> = vec![
            ("assignmentid".into(), assignid.to_string()),
            ("userid".into(), userid.to_string()),
            ("grade".into(), format!("{}", (grade * 100.0).round() / 100.0)),
            ("attemptnumber".into(), "-1".into()),
            ("addattempt".into(), "0".into()),
            ("workflowstate".into(), String::new()),
            ("applytoall".into(), "0".into()),
        ];
        if let Some(f) = feedback {
            p.push(("plugindata[assignfeedbackcomments_editor][text]".into(), f.to_string()));
            p.push(("plugindata[assignfeedbackcomments_editor][format]".into(), "1".into()));
        }
        self.call(http, "mod_assign_save_grade", &p).map(|_| ())
    }
}

pub struct MFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub mtime: u64,
}

pub struct Submission {
    pub userid: i64,
    pub status: String,
    pub modified: u64,
    pub files: Vec<MFile>,
    pub text: String,
}

/// Безопасное имя файла/папки (как в архиве Moodle, без запрещённых в Windows знаков).
pub fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let t = t.trim().trim_end_matches('.').to_string();
    if t.is_empty() { "файл".into() } else { t.chars().take(120).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_and_encoding() {
        assert_eq!(normalize_base("moodle.example.ru/course/view.php?id=5"), "https://moodle.example.ru");
        assert_eq!(normalize_base("http://127.0.0.1:8799/"), "http://127.0.0.1:8799");
        assert_eq!(urlenc("a b&ж"), "a+b%26%D0%B6");
        assert_eq!(safe_name("Работа: 1/2?.docx"), "Работа_ 1_2_.docx");
    }
}
