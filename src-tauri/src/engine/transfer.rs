//! Экспорт и импорт всего, что есть в приложении, одним файлом .proverka (zip):
//! настройки (по желанию — без ключей API), работы и результаты проверки, журнал групп,
//! кэш результатов, свои темы и шрифты, а при желании — и сами файлы заданий и работ.
//! Так приложение переносится на другой компьютер или сохраняется целиком.

use super::core::{now_s, Core};
use super::json::{self, Json};
use super::zip::{ZipArchive, ZipWriter};
use std::path::{Path, PathBuf};

pub const KIND: &str = "proverka-export";

#[derive(Clone, Copy, Debug)]
pub struct Parts {
    pub settings: bool,
    pub keys: bool,
    pub session: bool,
    pub journal: bool,
    pub results: bool,
    pub looks: bool,
    pub files: bool,
}

impl Parts {
    pub fn from_json(j: &Json) -> Parts {
        let b = |k: &str, d: bool| j.get(k).bool_or(d);
        Parts { settings: b("settings", true), keys: b("keys", false), session: b("session", true), journal: b("journal", true), results: b("results", true), looks: b("looks", true), files: b("files", false) }
    }
}

/// Все строки JSON, похожие на путь к существующему файлу.
fn file_paths(j: &Json, out: &mut Vec<String>) {
    match j {
        Json::Str(s) => {
            let looks_abs = s.starts_with('/') || (s.len() > 3 && s.as_bytes()[1] == b':' && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')) || s.starts_with("\\\\");
            if looks_abs && s.len() < 1000 && !out.contains(s) && Path::new(s).is_file() {
                out.push(s.clone());
            }
        }
        Json::Arr(v) => v.iter().for_each(|x| file_paths(x, out)),
        Json::Obj(v) => v.iter().for_each(|(_, x)| file_paths(x, out)),
        _ => {}
    }
}

fn add_dir(z: &mut ZipWriter, dir: &Path, prefix: &str) -> usize {
    let mut n = 0;
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    for e in rd.flatten() {
        let p = e.path();
        let name = format!("{}/{}", prefix, e.file_name().to_string_lossy());
        if p.is_dir() {
            n += add_dir(z, &p, &name);
        } else if let Ok(b) = std::fs::read(&p) {
            z.add(&name, &b);
            n += 1;
        }
    }
    n
}

fn json_str(s: &str) -> String {
    let mut o = String::new();
    json::escape_into(s, &mut o);
    o
}

/// Собрать файл экспорта.
pub fn export(core: &Core, dest: &str, parts: Parts) -> Result<Json, String> {
    if dest.trim().is_empty() {
        return Err("Не выбран файл".into());
    }
    core.save_session();
    core.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
    core.save_settings();
    let dd = core.data_dir.clone();
    let dd_s = dd.to_string_lossy().to_string();
    let mut z = ZipWriter::new();
    let mut files: Vec<Json> = Vec::new();
    let mut counts = Json::obj();
    if parts.settings || parts.looks {
        let mut st = core.settings();
        if !parts.keys {
            let provs: Vec<Json> = st
                .get("providers")
                .as_arr()
                .iter()
                .map(|p| {
                    let mut p = p.clone();
                    if !p.get("api_key").str_or("").is_empty() {
                        p.set("api_key", "");
                    }
                    p
                })
                .collect();
            st.set("providers", Json::Arr(provs));
            // ключ Moodle — тоже секрет
            let mut m = st.get("moodle_ws").clone();
            if !m.is_null() {
                m.set("token", "");
                st.set("moodle_ws", m);
            }
        }
        if !parts.settings {
            // только оформление
            let mut only = Json::obj();
            for k in ["theme", "look_style", "font_ui", "font_head", "font_size", "custom_themes", "custom_fonts", "sound", "sound_pack", "sound_volume", "animations"] {
                if !st.get(k).is_null() {
                    only.set(k, st.get(k).clone());
                }
            }
            st = only;
        }
        z.add_str("settings.json", &st.to_string());
    }
    if parts.session {
        let text = std::fs::read_to_string(dd.join("session.json")).unwrap_or_else(|_| "{}".into());
        let sess = json::parse(&text).unwrap_or(Json::Null);
        let mut paths = Vec::new();
        file_paths(&sess, &mut paths);
        let mut k = 0;
        for p in paths {
            let inside = Path::new(&p).starts_with(&dd);
            if !inside && !parts.files {
                continue;
            }
            let Ok(b) = std::fs::read(&p) else { continue };
            let entry = if inside {
                format!("data/{}", Path::new(&p).strip_prefix(&dd).map(|x| x.to_string_lossy().replace('\\', "/")).unwrap_or_default())
            } else {
                // папка и имя файла сохраняются: по ним приложение узнаёт группу, студента и вариант
                k += 1;
                let parent = Path::new(&p).parent().and_then(|x| x.file_name()).map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "_".into());
                format!("files/{:04}/{}/{}", k, parent, super::core::file_name(&p))
            };
            z.add(&entry, &b);
            files.push(Json::obj().with("orig", p.as_str()).with("entry", entry.as_str()));
        }
        z.add_str("session.json", &text);
        counts.set("works", sess.get("works").as_arr().len());
        counts.set("files", files.len());
    }
    if parts.journal {
        let text = std::fs::read_to_string(dd.join("journal.json")).unwrap_or_else(|_| "{}".into());
        z.add_str("journal.json", &text);
        counts.set("journal", json::parse(&text).map(|j| j.get("entries").as_arr().len()).unwrap_or(0));
        // сверка с оценками преподавателя и архив прошлых лет — часть истории журнала
        if let Ok(t) = std::fs::read_to_string(dd.join("calibration.json")) {
            z.add_str("data/calibration.json", &t);
        }
        counts.set("archive", add_dir(&mut z, &dd.join("archive"), "data/archive"));
    }
    if parts.results {
        counts.set("results", add_dir(&mut z, &dd.join("results"), "data/results"));
    }
    if parts.looks {
        counts.set("fonts", add_dir(&mut z, &dd.join("fonts"), "data/fonts"));
    }
    let manifest = Json::obj()
        .with("type", KIND)
        .with("version", 1)
        .with("created", now_s())
        .with("app", env!("CARGO_PKG_VERSION"))
        .with("data_dir", dd_s.as_str())
        .with(
            "parts",
            Json::obj()
                .with("settings", parts.settings)
                .with("keys", parts.keys && parts.settings)
                .with("session", parts.session)
                .with("journal", parts.journal)
                .with("results", parts.results)
                .with("looks", parts.looks)
                .with("files", parts.files),
        )
        .with("counts", counts.clone())
        .with("files", Json::Arr(files));
    z.add_str("manifest.json", &manifest.to_string());
    let bytes = z.finish();
    std::fs::write(dest, &bytes).map_err(|e| format!("не удалось сохранить: {}", e))?;
    Ok(Json::obj().with("path", dest).with("bytes", bytes.len()).with("counts", counts))
}

/// Что лежит в файле экспорта.
pub fn inspect(path: &str) -> Result<Json, String> {
    let data = std::fs::read(path).map_err(|e| format!("не удалось открыть: {}", e))?;
    let z = ZipArchive::open(&data).map_err(|_| "Это не файл экспорта «Проверки заданий»".to_string())?;
    let m = z.read_string("manifest.json").and_then(|t| json::parse(&t).ok()).ok_or("В файле нет описания (manifest.json) — это не экспорт приложения")?;
    if m.get("type").str_or("") != KIND {
        return Err("Это не файл экспорта «Проверки заданий»".into());
    }
    Ok(m)
}

/// Импорт выбранных частей. Текущее состояние запоминается — «Отменить» вернёт его.
pub fn import(core: &Core, path: &str, want: Parts) -> Result<Json, String> {
    let m = inspect(path)?;
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let z = ZipArchive::open(&data)?;
    let has = Parts::from_json(m.get("parts"));
    let dd = core.data_dir.clone();
    core.snapshot("импорт из файла");
    let mut done: Vec<&str> = Vec::new();
    // файлы: внутренние — на свои места в папке данных, внешние — в imported/<время>
    let old_dd = m.get("data_dir").str_or("");
    let stamp = now_s();
    let mut remap: Vec<(String, String)> = Vec::new();
    if want.session && has.session {
        for f in m.get("files").as_arr() {
            let (orig, entry) = (f.get("orig").str_or(""), f.get("entry").str_or(""));
            if entry.contains("..") {
                continue;
            }
            let Some(b) = z.read(&entry) else { continue };
            let target: PathBuf = if let Some(rel) = entry.strip_prefix("data/") {
                dd.join(rel)
            } else {
                dd.join("imported").join(stamp.to_string()).join(entry.trim_start_matches("files/"))
            };
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&target, &b).is_ok() {
                remap.push((orig, target.to_string_lossy().to_string()));
            }
        }
        if let Some(mut text) = z.read_string("session.json") {
            // длинные пути — раньше коротких
            remap.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
            for (from, to) in &remap {
                text = text.replace(&json_str(from), &json_str(to));
            }
            if !old_dd.is_empty() {
                text = text.replace(&json_str(&old_dd), &json_str(&dd.to_string_lossy()));
            }
            std::fs::write(dd.join("session.json"), text).map_err(|e| e.to_string())?;
            done.push("работы и результаты");
        }
    }
    if want.journal && has.journal {
        if let Some(t) = z.read_string("journal.json") {
            std::fs::write(dd.join("journal.json"), t).map_err(|e| e.to_string())?;
            done.push("журнал групп");
        }
    }
    let unpack = |prefix: &str| -> usize {
        let mut n = 0;
        for name in z.names() {
            if let Some(rel) = name.strip_prefix(prefix) {
                if rel.is_empty() || rel.contains("..") || name.ends_with('/') {
                    continue;
                }
                if let Some(b) = z.read(name) {
                    let t = dd.join(prefix.trim_start_matches("data/")).join(rel);
                    if let Some(p) = t.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    if std::fs::write(&t, b).is_ok() {
                        n += 1;
                    }
                }
            }
        }
        n
    };
    if want.journal && has.journal {
        if let Some(t) = z.read_string("data/calibration.json") {
            let _ = std::fs::write(dd.join("calibration.json"), t);
        }
        if unpack("data/archive/") > 0 {
            done.push("архив прошлых лет");
        }
    }
    if want.results && has.results && unpack("data/results/") > 0 {
        done.push("кэш результатов");
    }
    if (want.settings && has.settings) || (want.looks && has.looks) {
        if let Some(t) = z.read_string("settings.json") {
            let incoming = json::parse(&t).map_err(|_| "повреждены настройки в файле".to_string())?;
            let mut cur = core.settings();
            if want.settings && has.settings {
                // ключи API: если в файле их нет — оставляем свои
                let mine = cur.get("providers").clone();
                let mut next = incoming.clone();
                let provs: Vec<Json> = incoming
                    .get("providers")
                    .as_arr()
                    .iter()
                    .map(|p| {
                        let mut p = p.clone();
                        if p.get("api_key").str_or("").is_empty() {
                            if let Some(old) = mine.as_arr().iter().find(|o| o.get("id").str_or("") == p.get("id").str_or("")) {
                                p.set("api_key", old.get("api_key").clone());
                            }
                        }
                        p
                    })
                    .collect();
                if !provs.is_empty() {
                    next.set("providers", Json::Arr(provs));
                }
                if incoming.get("moodle_ws").get("token").str_or("").is_empty() && !cur.get("moodle_ws").is_null() {
                    next.set("moodle_ws", cur.get("moodle_ws").clone());
                }
                if !(want.looks && has.looks) {
                    for k in ["theme", "look_style", "font_ui", "font_head", "font_size", "custom_themes", "custom_fonts"] {
                        next.set(k, cur.get(k).clone());
                    }
                }
                cur = next;
                done.push("настройки");
            } else {
                for k in ["theme", "look_style", "font_ui", "font_head", "font_size", "custom_themes", "custom_fonts"] {
                    if !incoming.get(k).is_null() {
                        cur.set(k, incoming.get(k).clone());
                    }
                }
            }
            if want.looks && has.looks {
                unpack("data/fonts/");
                done.push("оформление");
            }
            std::fs::write(dd.join("settings.json"), cur.to_string()).map_err(|e| e.to_string())?;
        }
    }
    core.reload_from_disk();
    core.log(&format!("Импорт из файла: {}", done.join(", ")));
    Ok(Json::obj().with("imported", done.into_iter().map(String::from).collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_parts() {
        let dir = std::env::temp_dir().join(format!("prv_tr_{}", now_s()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        let j = Json::obj().with("works", Json::Arr(vec![Json::obj().with("path", f.to_string_lossy().to_string()).with("name", "a.txt")]));
        let mut out = Vec::new();
        file_paths(&j, &mut out);
        assert_eq!(out, vec![f.to_string_lossy().to_string()]);
        let p = Parts::from_json(&Json::obj());
        assert!(p.settings && !p.keys && !p.files);
        let _ = std::fs::remove_dir_all(dir);
    }
}
