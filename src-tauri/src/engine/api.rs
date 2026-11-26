//! Команды интерфейса: `dispatch(method, payload_json) -> json`.

use super::core::{natural_key, Core, Outcome};
use super::extras;
use super::journal::Entry;
use super::report::Block;
use super::grader;
use super::json::{self, Json};
use super::llm::{self, Provider};
use super::mcp::{self, McpClient};
use super::report;
use super::settings;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub fn dispatch(core: &Arc<Core>, method: &str, payload: &str) -> Result<String, String> {
    let p = if payload.trim().is_empty() { Json::obj() } else { json::parse(payload)? };
    // часовой пояс компьютера (для дат в ведомости) — интерфейс присылает с каждым вызовом
    if let Some(tz) = p.get("tz").as_f64() {
        report::TZ_MIN.store(tz as i64, Ordering::Relaxed);
    }
    // паника внутри метода не должна ронять приложение и блокировать данные
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(core, method, &p))).map_err(|e| {
        let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        core.log(&format!("Внутренняя ошибка в «{}»: {}", method, msg));
        format!("Внутренняя ошибка ({}). Подробности — в журнале MCP на шаге «Проверка».", msg)
    })??;
    Ok(r.to_string())
}

/// Команды, которые меняют то, что сейчас проверяется. Во время проверки они запрещены.
const LOCKED_WHILE_RUNNING: &[&str] = &[
    "add_paths",
    "remove",
    "clear",
    "set_doc_kind",
    "set_work_assignment",
    "set_work_group",
    "set_task_scope",
    "set_pdf_text",
    "set_pages",
    "set_source_text",
    "set_page_image",
    "merge_works",
    "reset_settings",
    "group_save",
    "moodle_import",
    "llama_start",
    "ai_decide",
    "group_delete",
    "journal_roster",
    "lmstudio_reload",
    "add_photo_work",
    "journal_student",
    "undo",
    "backup_restore",
    "local_stop",
    "import_all",
    "update_apply",
    "rubric_set",
];

pub const LOCK_MESSAGE: &str = "Нейросеть сейчас проверяет работы — изменения недоступны. Дождитесь окончания или нажмите «Остановить».";

fn handle(core: &Arc<Core>, method: &str, p: &Json) -> Result<Json, String> {
    if core.running.load(Ordering::SeqCst) {
        let settings_lock = method == "set_settings" && {
            // во время проверки можно менять только то, что не влияет на неё: оформление, звук, трей
            let s = p.get("settings");
            s.as_obj().iter().any(|(k, _)| !matches!(k.as_str(), "theme" | "look_style" | "font_ui" | "font_head" | "font_size" | "sound" | "sound_volume" | "sound_pack" | "animations" | "tray" | "notify" | "updates" | "ui" | "custom_themes" | "custom_styles" | "custom_fonts" | "llama" | "teacher" | "programs" | "journal_auto" | "sound_events" | "journal_opts" | "csv_encoding" | "csv_sep" | "subjects" | "reminders" | "backup"))
        };
        if LOCKED_WHILE_RUNNING.contains(&method) || settings_lock {
            return Err(LOCK_MESSAGE.into());
        }
    }
    // перед удалением запоминаем состояние — его вернёт «Отменить»
    let undoable = match method {
        "remove" => Some("удаление"),
        "clear" => Some(if p.get("kind").str_or("") == "tasks" { "очистка заданий" } else { "очистка работ" }),
        "merge_works" => Some("объединение работ"),
        "group_delete" => Some("удаление группы"),
        "journal_clear" => Some("очистка оценок группы"),
        "journal_student" => Some(if p.get("action").str_or("") == "delete" { "удаление студента" } else { "переименование студента" }),
        "journal_roster" if p.get("names").as_arr().is_empty() => Some("удаление списка группы"),
        "journal_topic" if p.get("action").str_or("") == "delete" => Some("удаление темы"),
        "journal_fill" => Some("«2» несдавшим"),
        _ => None,
    };
    if let Some(what) = undoable {
        core.snapshot(what);
    }
    let res = handle_inner(core, method, &p);
    // удалили работы — распакованные из архивов Moodle файлы больше не нужны
    if res.is_ok() && matches!(method, "remove" | "clear" | "merge_works" | "undo" | "backup_restore") {
        core.cleanup_moodle();
    }
    res
}

fn handle_inner(core: &Arc<Core>, method: &str, p: &Json) -> Result<Json, String> {
    match method {
        "state" => Ok(core.state_json()),

        "undo" => {
            let what = core.undo()?;
            Ok(core.state_json().with("undone", what))
        }

        "backups" => Ok(Json::obj().with("list", core.backups()).with("dir", core.data_dir.join("backups").to_string_lossy().to_string())),

        "backup_restore" => {
            core.restore_backup(&p.get("name").str_or(""))?;
            Ok(core.state_json())
        }

        "backup_now" => {
            let _ = std::fs::remove_dir_all(core.data_dir.join("backups").join(super::core::ymd(super::core::now_s() as i64 + super::report::TZ_MIN.load(Ordering::Relaxed) * 60)));
            core.save_session();
            core.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
            core.backup_daily();
            // и в свою папку — заново, даже если сегодня копия уже была
            let ext = core.backup_external(true);
            Ok(Json::obj().with("list", core.backups()).with("external", match &ext { Ok(Some(p)) => Json::from(p.as_str()), _ => Json::Null }).with("external_error", match &ext { Err(e) => Json::from(e.as_str()), _ => Json::Null }))
        }

        "add_paths" => {
            let kind = p.get("kind").str_or("works");
            let paths = p.get("paths").str_list();
            let errors = if kind == "tasks" { core.add_tasks(&paths, None, &p.get("scope").str_or("")) } else { core.add_works(&paths, None, &p.get("group").str_or("")) };
            core.compute_plagiarism();
            Ok(core.state_json().with("errors", errors))
        }

        "remove" => {
            let id = p.get("id").str_or("");
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if p.get("kind").str_or("") == "tasks" {
                    ws.tasks.retain(|t| t.id != id);
                    let mut w2 = std::mem::take(&mut *ws);
                    core.rebuild_sets(&mut w2);
                    *ws = w2;
                } else {
                    ws.works.retain(|w| w.id != id);
                    ws.outcomes.remove(&id);
                }
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(core.state_json())
        }

        "clear" => {
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                match p.get("kind").str_or("").as_str() {
                    "tasks" => {
                        ws.tasks.clear();
                        ws.sets.clear();
                    }
                    "results" => {
                        for o in ws.outcomes.values_mut() {
                            *o = Outcome { status: "pending".into(), ..Default::default() };
                        }
                    }
                    _ => {
                        ws.works.clear();
                        ws.outcomes.clear();
                        ws.plag.clear();
                    }
                }
            }
            core.save_session();
            Ok(core.state_json())
        }

        "set_doc_kind" => {
            let id = p.get("id").str_or("");
            let kind = super::assign::DocKind::from_str(&p.get("kind").str_or("task"));
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(t) = ws.tasks.iter_mut().find(|t| t.id == id) {
                    t.kind = kind;
                }
                if let Some(code) = p.get("code").as_str() {
                    if let Some(t) = ws.tasks.iter_mut().find(|t| t.id == id) {
                        t.code = code.to_string();
                    }
                }
                let mut w2 = std::mem::take(&mut *ws);
                core.rebuild_sets(&mut w2);
                *ws = w2;
            }
            core.save_session();
            Ok(core.state_json())
        }

        "set_work_assignment" => {
            let id = p.get("id").str_or("");
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(w) = ws.works.iter_mut().find(|w| w.id == id || id == "*") {
                    w.assignment = p.get("code").as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
                }
                if id == "*" {
                    let code = p.get("code").as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
                    for w in ws.works.iter_mut() {
                        w.assignment = code.clone();
                    }
                }
            }
            core.save_session();
            Ok(core.state_json())
        }

        "set_work_group" => {
            let ids = p.get("ids").str_list();
            let group = p.get("group").str_or("").trim().to_string();
            if group.is_empty() {
                return Err("Укажите группу".into());
            }
            let mut moved: Vec<(String, String)> = Vec::new();
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                for w in ws.works.iter_mut() {
                    if ids.contains(&w.id) && w.group != group {
                        moved.push((w.group.clone(), w.name.clone()));
                        w.group = group.clone();
                    }
                }
            }
            // оценки этих работ, уже записанные в журнал, переезжают вместе с ними
            if !moved.is_empty() {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                let mut changed = false;
                for e in j.entries.iter_mut() {
                    if moved.iter().any(|(g, f)| &e.group == g && &e.file == f) {
                        e.group = group.clone();
                        changed = true;
                    }
                }
                if changed {
                    j.save();
                }
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(core.state_json())
        }

        "set_task_scope" => {
            // для кого задания: по id набора (все его файлы) или по id файлов
            let scope = p.get("scope").str_or("").trim().to_string();
            let set_id = p.get("set").str_or("");
            let ids = p.get("ids").str_list();
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let set_files: Vec<(String, String)> = ws.sets.iter().filter(|s| s.id == set_id).map(|s| (s.code.clone(), s.scope.clone())).collect();
                for t in ws.tasks.iter_mut() {
                    let in_set = set_files.iter().any(|(c, sc)| &t.code == c && &t.scope == sc);
                    if ids.contains(&t.id) || in_set {
                        t.scope = scope.clone();
                    }
                }
                let mut w2 = std::mem::take(&mut *ws);
                core.rebuild_sets(&mut w2);
                *ws = w2;
            }
            core.save_session();
            Ok(core.state_json())
        }

        "group_save" => {
            let name = p.get("name").str_or("").trim().to_string();
            let old = p.get("old_name").str_or("").trim().to_string();
            if name.is_empty() {
                return Err("Укажите название группы".into());
            }
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                if !old.is_empty() && old != name {
                    if j.groups.iter().any(|g| g.name == name) {
                        return Err(format!("Группа «{}» уже есть", name));
                    }
                    j.rename_group(&old, &name);
                }
                let program = p.get("program").str_or("").trim().to_string();
                let note = p.get("note").str_or("");
                match j.groups.iter_mut().find(|g| g.name == name) {
                    Some(g) => {
                        g.program = program;
                        g.note = note;
                    }
                    None => j.groups.push(super::journal::GroupInfo { name: name.clone(), program, note }),
                }
                if !p.get("roster").is_null() {
                    let names: Vec<String> = p
                        .get("roster")
                        .str_list()
                        .into_iter()
                        .map(|n| n.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ' || c == '\t').trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect();
                    if names.is_empty() {
                        j.rosters.remove(&name);
                        j.assigned.remove(&name);
                        j.teams.remove(&name);
                    } else {
                        if let Some(m) = j.assigned.get_mut(&name) {
                            m.retain(|k, _| names.contains(k));
                        }
                        if let Some(m) = j.teams.get_mut(&name) {
                            m.retain(|k, _| names.contains(k));
                        }
                        j.rosters.insert(name.clone(), names);
                    }
                }
                j.save();
            }
            if !old.is_empty() && old != name {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                for w in ws.works.iter_mut() {
                    if w.group == old {
                        w.group = name.clone();
                    }
                }
                for t in ws.tasks.iter_mut() {
                    if t.scope == old {
                        t.scope = name.clone();
                    }
                }
                let mut w2 = std::mem::take(&mut *ws);
                core.rebuild_sets(&mut w2);
                *ws = w2;
            }
            core.save_session();
            Ok(core.state_json())
        }

        "group_delete" => {
            // с entries=true — вместе с оценками журнала, темами и календарём
            let name = p.get("name").str_or("");
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                j.delete_group(&name, p.get("entries").bool_or(false));
                j.save();
            }
            Ok(core.state_json().with("journal", journal_view(core, &p)))
        }

        "programs_save" => {
            let list = p.get("programs").str_list();
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                st.set("programs", list);
            }
            core.save_settings();
            Ok(core.state_json())
        }

        "set_pdf_text" => {
            let id = p.get("id").str_or("");
            let text = p.get("text").str_or("");
            // pdf.js не смог открыть файл (повреждён, защищён паролем) — не блокируем проверку
            let failed = p.get("failed").str_or("");
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(w) = ws.works.iter_mut().find(|w| w.id == id) {
                    w.notes.retain(|n| !n.contains("PDF"));
                    if !failed.is_empty() {
                        w.needs_pdf = false;
                        w.notes.push(format!("Не удалось прочитать PDF: {} — пересохраните файл или попросите студента прислать .docx", failed));
                    } else {
                        let src = std::mem::take(&mut w.sources);
                        let main = w.path.clone();
                        w.pages.retain(|pg| pg.src == main);
                        w.transcript.clear();
                        w.transcribed = false;
                        w.visible = super::text::tidy(&text);
                        w.marked = w.visible.clone();
                        w.needs_pdf = w.visible.trim().is_empty() && w.pages.is_empty();
                        w.notes.push("Текст извлечён из PDF; скрытый текст в PDF не определяется".into());
                        core.attach_sources(w, &src);
                    }
                }
                if let Some(t) = ws.tasks.iter_mut().find(|t| t.id == id) {
                    if !failed.is_empty() {
                        t.needs_pdf = false;
                        t.notes.push(format!("Не удалось прочитать PDF: {}", failed));
                    } else {
                        t.marked = super::text::tidy(&text);
                        t.notes.push("Текст извлечён из PDF; скрытые ловушки в PDF не определяются — загрузите .docx".into());
                    }
                }
                let mut w2 = std::mem::take(&mut *ws);
                core.rebuild_sets(&mut w2);
                *ws = w2;
            }
            core.compute_plagiarism();
            Ok(Json::obj().with("ok", true))
        }

        // страницы скана PDF, отрисованные интерфейсом в JPEG
        "set_pages" => {
            let id = p.get("id").str_or("");
            let images: Vec<Vec<u8>> = p.get("images").str_list().iter().map(|x| b64_decode(x)).filter(|b| !b.is_empty()).collect();
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let w = ws.works.iter_mut().find(|w| w.id == id).ok_or("работа не найдена")?;
                if images.is_empty() {
                    w.needs_pdf = false;
                    w.notes.push("В PDF нет ни текста, ни страниц, которые удалось отрисовать".into());
                } else {
                    core.set_pdf_pages(w, &images);
                }
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(Json::obj().with("ok", true))
        }

        // текст (и страницы скана) PDF, присоединённого к работе вторым файлом
        "set_source_text" => {
            let id = p.get("id").str_or("");
            let idx = p.get("i").i64_or(0).max(0) as usize;
            let images: Vec<Vec<u8>> = p.get("images").str_list().iter().map(|x| b64_decode(x)).filter(|b| !b.is_empty()).collect();
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let w = ws.works.iter_mut().find(|w| w.id == id).ok_or("работа не найдена")?;
                core.set_source_pdf(w, idx, &p.get("text").str_or(""), &images, &p.get("failed").str_or(""));
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(Json::obj().with("ok", true))
        }

        // уменьшенное интерфейсом большое фото (или failed — отправим как есть)
        "set_page_image" => {
            let id = p.get("id").str_or("");
            let idx = p.get("i").i64_or(0).max(0) as usize;
            let data = b64_decode(&p.get("b64").str_or(""));
            let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let w = ws.works.iter_mut().find(|w| w.id == id).ok_or("работа не найдена")?;
            core.set_page_image(w, idx, if data.is_empty() { None } else { Some(&data) });
            Ok(Json::obj().with("ok", true))
        }

        // несколько файлов (страницы-фото, второй документ) — одна работа
        "merge_works" => {
            let ids = p.get("ids").str_list();
            if ids.len() < 2 {
                return Err("Выберите хотя бы две работы, которые нужно объединить".into());
            }
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let mut extra: Vec<String> = Vec::new();
                for id in &ids[1..] {
                    if let Some(w) = ws.works.iter().find(|w| &w.id == id) {
                        extra.push(w.path.clone());
                        extra.extend(w.sources.iter().cloned());
                    }
                }
                ws.works.retain(|w| !ids[1..].contains(&w.id));
                for id in &ids[1..] {
                    ws.outcomes.remove(id);
                }
                let main = ws.works.iter_mut().find(|w| w.id == ids[0]).ok_or("работа не найдена")?;
                core.attach_sources(main, &extra);
                if let Some(o) = ws.outcomes.get_mut(&ids[0]) {
                    *o = super::core::Outcome { status: "pending".into(), ..Default::default() };
                }
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(core.state_json())
        }

        "read_file_b64" => {
            let id = p.get("id").str_or("");
            let page = p.get("page").as_f64().map(|x| x as i64);
            if let Some(si) = p.get("source").as_f64() {
                let path = {
                    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    ws.works.iter().find(|w| w.id == id).and_then(|w| w.pending_pdf.get(si.max(0.0) as usize).cloned())
                }
                .ok_or("файл не найден")?;
                let data = std::fs::read(&path).map_err(|e| e.to_string())?;
                return Ok(Json::obj().with("b64", b64(&data)).with("name", super::core::file_name(&path).as_str()));
            }
            let path = {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                match page {
                    Some(i) => ws.works.iter().find(|w| w.id == id).and_then(|w| w.pages.get(i.max(0) as usize)).map(|pg| pg.file.clone()),
                    None => ws.works.iter().find(|w| w.id == id).map(|w| w.path.clone()).or_else(|| ws.tasks.iter().find(|t| t.id == id).map(|t| t.path.clone())),
                }
            }
            .ok_or("файл не найден")?;
            let b = std::fs::read(&path).map_err(|e| e.to_string())?;
            Ok(Json::obj().with("b64", b64(&b)))
        }

        "work" => {
            let id = p.get("id").str_or("");
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let w = ws.works.iter().find(|w| w.id == id).ok_or("работа не найдена")?;
            Ok(core.work_json(&ws, w, true))
        }

        "assignment" => {
            let code = p.get("code").str_or("");
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let s = ws.sets.iter().find(|s| s.id == code).or_else(|| ws.sets.iter().find(|s| s.code == code)).ok_or("набор не найден")?;
            Ok(s.to_json(true))
        }

        "set_settings" => {
            let new = p.get("settings").clone();
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                *st = settings::merge(&st.clone(), &new);
                // списки заменяем целиком (merge их не сливает поэлементно)
                for k in ["providers", "caps", "custom_markers"] {
                    if !new.get(k).is_null() {
                        st.set(k, new.get(k).clone());
                    }
                }
            }
            core.save_settings();
            // оболочка обновляет меню трея (звук, вид прогресса)
            (core.emit)("settings_saved", "{}".to_string());
            // маркеры влияют на наборы заданий
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let mut w2 = std::mem::take(&mut *ws);
                core.rebuild_sets(&mut w2);
                *ws = w2;
            }
            // порог совпадения и «сколько одинаковых можно зачесть» — пересчитываем итоги
            if !new.get("penalties").is_null() || !new.get("archive").is_null() {
                core.compute_plagiarism();
            }
            // ограничения оценки применяются и к уже проверенным работам
            if !new.get("penalties").is_null() || !new.get("rules").is_null() || !new.get("caps").is_null() || !new.get("half_grades").is_null() {
                core.refinalize_all();
                core.save_session();
            }
            Ok(core.settings())
        }

        "reset_settings" => {
            *core.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings::defaults();
            core.save_settings();
            Ok(core.settings())
        }

        "list_models" => {
            let prov = Provider::from_json(p.get("provider"));
            let models = llm::list_models(core.http.as_ref(), &prov)?;
            Ok(Json::obj().with("models", models))
        }

        "provider_info" => {
            // контекст: загруженный и максимальный (для LM Studio — по всем моделям)
            let prov = Provider::from_json(p.get("provider"));
            let info = if prov.kind == "lmstudio" { ctx_info_json(core, &prov) } else { Json::arr() };
            let current = grader::detect_context(core, &prov);
            Ok(Json::obj().with("models", info).with("current_ctx", current).with("kind", prov.kind.as_str()))
        }

        "lmstudio_reload" => {
            let prov = Provider::from_json(p.get("provider"));
            let ctx = p.get("ctx").i64_or(16384).clamp(1024, 1_048_576);
            if prov.model.is_empty() {
                return Err("Сначала выберите модель".into());
            }
            let out = grader::lmstudio_reload(&prov.model, ctx)?;
            let now = grader::detect_context(core, &prov);
            Ok(Json::obj().with("output", out.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>()).with("current_ctx", now))
        }

        "test_provider" => {
            let prov = Provider::from_json(p.get("provider"));
            Ok(grader::test_provider(core, &prov))
        }

        "detect_local" => {
            let mut out = Json::arr();
            for (kind, url) in [("lmstudio", "http://localhost:1234/v1"), ("ollama", "http://localhost:11434"), ("llamacpp", "http://localhost:8080/v1")] {
                let prov = Provider::from_json(&Json::obj().with("kind", kind).with("base_url", url));
                let r = llm::list_models(core.http.as_ref(), &prov);
                let info = if kind == "lmstudio" && r.is_ok() { ctx_info_json(core, &prov) } else { Json::arr() };
                out.push(
                    Json::obj()
                        .with("kind", kind)
                        .with("url", url)
                        .with("online", r.is_ok())
                        .with("models", r.clone().unwrap_or_default())
                        .with("ctx", info)
                        .with("error", r.err()),
                );
            }
            Ok(out)
        }

        "start_check" => {
            if core.running.load(Ordering::SeqCst) {
                return Err("Проверка уже идёт".into());
            }
            let mut ids = p.get("ids").str_list();
            let only_new = p.get("only_unchecked").bool_or(false);
            {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if ws.works.is_empty() {
                    return Err("Сначала загрузите работы студентов".into());
                }
                if ids.is_empty() {
                    ids = ws
                        .works
                        .iter()
                        .filter(|w| !only_new || ws.outcomes.get(&w.id).map(|o| o.status != "done").unwrap_or(true))
                        .map(|w| w.id.clone())
                        .collect();
                }
                // PDF, которые интерфейс ещё не успел прочитать, — проверяются без них
                let pdf_wait: Vec<String> = ws.works.iter().filter(|w| ids.contains(&w.id) && w.needs_pdf && w.visible.is_empty() && w.pages.is_empty()).map(|w| w.id.clone()).collect();
                if !pdf_wait.is_empty() && pdf_wait.len() == ids.len() {
                    return Err("PDF-файлы ещё читаются — подождите пару секунд и запустите проверку снова".into());
                }
                ids.retain(|i| !pdf_wait.contains(i));
            }
            if ids.is_empty() {
                return Err("Нет работ для проверки".into());
            }
            if grader::active_provider(core).map(|p| p.model.is_empty()).unwrap_or(true) {
                return Err("Не выбрана модель — откройте раздел «Модель» и выберите её".into());
            }
            // «проверить заново»: не брать результаты прошлых проверок
            core.no_cache.store(p.get("fresh").bool_or(false), Ordering::SeqCst);
            let c = core.clone();
            let n = ids.len();
            std::thread::spawn(move || grader::run(c, ids));
            Ok(Json::obj().with("started", true).with("count", n))
        }

        // снять работы с идущей проверки: из очереди — сразу, проверяемую — прервать
        "dequeue" => {
            let ids = p.get("ids").str_list();
            let mut removed = 0;
            for id in &ids {
                let was_queued = {
                    let mut q = core.queue.lock().unwrap_or_else(|e| e.into_inner());
                    let before = q.len();
                    q.retain(|x| x != id);
                    before != q.len()
                };
                let st = {
                    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    ws.outcomes.get(id).map(|o| (o.status.clone(), o.grade.is_some())).unwrap_or_default()
                };
                if was_queued {
                    super::grader::emit_progress(core, id, if st.1 { "done" } else { "pending" }, "removed", "Снята с проверки");
                    removed += 1;
                } else if st.0 == "running" {
                    core.dropped.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone());
                    removed += 1;
                } else {
                    continue;
                }
                let t = core.run_total.load(Ordering::SeqCst);
                core.run_total.store(t.saturating_sub(1), Ordering::SeqCst);
            }
            Ok(Json::obj().with("removed", removed).with("total", core.run_total.load(Ordering::SeqCst)))
        }

        // добавить работы в идущую проверку
        "enqueue" => {
            if !core.running.load(Ordering::SeqCst) {
                return Err("Проверка не идёт".into());
            }
            let ids = p.get("ids").str_list();
            let mut added = 0;
            for id in ids {
                let busy = {
                    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    let exists = ws.works.iter().any(|w| w.id == id);
                    !exists || ws.outcomes.get(&id).map(|o| o.status == "queued" || o.status == "running").unwrap_or(false)
                };
                if busy {
                    continue;
                }
                core.dropped.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                core.queue.lock().unwrap_or_else(|e| e.into_inner()).push(id.clone());
                super::grader::emit_progress(core, &id, "queued", "queue", "");
                core.run_total.fetch_add(1, Ordering::SeqCst);
                added += 1;
            }
            Ok(Json::obj().with("added", added).with("total", core.run_total.load(Ordering::SeqCst)))
        }

        "cancel_check" => {
            core.cancel.store(true, Ordering::SeqCst);
            Ok(Json::obj().with("ok", true))
        }

        "set_override" => {
            let id = p.get("id").str_or("");
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let o = ws.outcomes.entry(id).or_default();
                // оценка преподавателя — как он её поставил, до сотых (2,7; 2,9…)
                o.override_grade = p.get("grade").as_f64().map(|g| (g.clamp(2.0, 5.0) * 100.0).round() / 100.0);
                o.override_comment = p.get("comment").str_or("");
                // преподаватель сам отмечает ИИ / списывание или снимает нарушение
                if let Some(v) = p.get("violation").as_str() {
                    if !["", "ai", "plagiarism", "none"].contains(&v) {
                        return Err("неизвестное нарушение".into());
                    }
                    o.override_violation = v.to_string();
                    o.apply_violation_override();
                    if o.status != "done" && o.override_violation != "" && o.override_violation != "none" && o.checked_at == 0 {
                        o.checked_at = super::core::now_s();
                    }
                }
            }
            core.save_session();
            Ok(core.state_json())
        }

        "ai_decide" => {
            // решение преподавателя по подозрению на ИИ: keep — по содержанию, two — «2», "" — отменить
            let decision = p.get("decision").str_or("");
            if !["", "keep", "two"].contains(&decision.as_str()) {
                return Err("неизвестное решение".into());
            }
            let mut ids = p.get("ids").str_list();
            if let Some(id) = p.get("id").as_str() {
                ids.push(id.to_string());
            }
            for id in &ids {
                {
                    let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(o) = ws.outcomes.get_mut(id) {
                        o.ai_decision = decision.clone();
                    }
                }
                core.refinalize(id)?;
            }
            core.save_session();
            if core.settings().get("journal_auto").bool_or(false) {
                core.journal_add_current();
            }
            Ok(core.state_json())
        }

        "report_preview" => {
            if let Some(tz) = p.get("tz").as_f64() {
                report::TZ_MIN.store(tz as i64, Ordering::Relaxed);
            }
            let (rows, stats) = report::rows(core, &p.get("group").str_or(""));
            let list: Vec<Json> = rows
                .iter()
                .map(|r| {
                    Json::obj()
                        .with("n", r.n)
                        .with("id", r.id.as_str())
                        .with("file", r.file.as_str())
                        .with("group", r.group.as_str())
                        .with("variant", r.variant.as_str())
                        .with("violation", r.violation.as_str())
                        .with("grade", r.grade_num)
                        .with("comment", r.comment.as_str())
                })
                .collect();
            Ok(Json::obj().with("rows", list).with("stats", stats))
        }

        "export" => {
            if let Some(tz) = p.get("tz").as_f64() {
                report::TZ_MIN.store(tz as i64, Ordering::Relaxed);
            }
            let fmt = p.get("format").str_or("docx");
            let path = p.get("path").str_or("");
            if path.is_empty() {
                return Err("не указан путь".into());
            }
            let bytes = match fmt.as_str() {
                "xlsx" => report::xlsx(core, &p.get("group").str_or("")),
                "csv" => report::csv(core, &p.get("group").str_or("")),
                _ => report::docx(core, &p.get("group").str_or("")),
            };
            std::fs::write(&path, bytes).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path))
        }

        "mcp_info" => {
            let client = McpClient::new(core.clone(), "инспектор");
            let init = client.initialize()?;
            let tools = client.list_tools()?;
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "ProverkaZadaniy.exe".into());
            Ok(Json::obj()
                .with("server", init.get("serverInfo").clone())
                .with("protocol", init.get("protocolVersion").clone())
                .with("tools", Json::Arr(tools))
                .with("exe", exe.as_str())
                .with("config", mcp::client_config(&exe)))
        }

        "mcp_call" => {
            let client = McpClient::new(core.clone(), "инспектор");
            client.raw(&p.get("method").str_or("tools/list"), p.get("params").clone())
        }

        "prompt_preview" => {
            let id = p.get("id").str_or("");
            let client = McpClient::new(core.clone(), "предпросмотр");
            let max = grader::active_provider(core).map(|p| grader::budget(&p).0).unwrap_or(40000);
            let mode = core.settings().get("compress").str_or("dense");
            let prompt = grader::build_direct_prompt_with(&client, &id, max.saturating_sub(grader::SYSTEM_DIRECT.chars().count()), if mode == "summary" { "dense" } else { &mode }, None)?;
            Ok(Json::obj().with("system", grader::SYSTEM_DIRECT).with("user", prompt))
        }

        // ---------------------------------------------------------- защита и отзывы
        "oral" => {
            let id = p.get("id").str_or("");
            if !p.get("regenerate").bool_or(false) {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(o) = ws.outcomes.get(&id) {
                    if !o.oral.get("questions").as_arr().is_empty() {
                        return Ok(o.oral.clone());
                    }
                }
            }
            extras::oral_questions(core, &id)
        }

        "feedback" => {
            let id = p.get("id").str_or("");
            let mode = p.get("mode").str_or("get");
            let text = match mode.as_str() {
                "ai" => extras::feedback_ai(core, &id)?,
                "template" => core.feedback_template(&id).ok_or("работа не найдена")?,
                "save" => {
                    let t = p.get("text").str_or("");
                    let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(o) = ws.outcomes.get_mut(&id) {
                        o.feedback = t.clone();
                    }
                    drop(ws);
                    core.save_session();
                    t
                }
                _ => {
                    let saved = core.ws.lock().unwrap_or_else(|e| e.into_inner()).outcomes.get(&id).map(|o| o.feedback.clone()).unwrap_or_default();
                    if saved.trim().is_empty() {
                        core.feedback_template(&id).ok_or("работа не найдена")?
                    } else {
                        saved
                    }
                }
            };
            Ok(Json::obj().with("text", text))
        }

        "autostart" => {
            // запуск вместе с Windows: запись в HKCU\...\Run (без прав администратора)
            if !cfg!(windows) {
                return Ok(Json::obj().with("supported", false).with("enabled", false).with("minimized", false));
            }
            const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
            const NAME: &str = "ProverkaZadaniy";
            let reg = |args: &[&str]| -> Result<String, String> {
                let mut c = std::process::Command::new("reg");
                c.args(args);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    c.creation_flags(0x08000000);
                }
                let o = c.output().map_err(|e| e.to_string())?;
                let t = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if o.status.success() { Ok(t) } else { Err(t) }
            };
            if p.get("action").str_or("get") == "set" {
                if p.get("enabled").bool_or(false) {
                    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                    let mut val = format!("\"{}\"", exe.display());
                    if p.get("minimized").bool_or(true) {
                        val.push_str(" --tray");
                    }
                    reg(&["add", KEY, "/v", NAME, "/t", "REG_SZ", "/d", &val, "/f"]).map_err(|e| format!("не удалось включить автозапуск: {}", e.trim()))?;
                } else {
                    let _ = reg(&["delete", KEY, "/v", NAME, "/f"]);
                }
            }
            let cur = reg(&["query", KEY, "/v", NAME]).unwrap_or_default();
            let on = cur.contains(NAME);
            Ok(Json::obj().with("supported", true).with("enabled", on).with("minimized", on && cur.contains("--tray")))
        }

        "app_info" => Ok(Json::obj()
            .with("version", env!("CARGO_PKG_VERSION"))
            // номер и дата сборки — их записывает tools/build.ps1 в src-tauri/build_info.txt
            .with("build", option_env!("PROVERKA_BUILD").unwrap_or(""))
            .with("data_dir", core.data_dir.to_string_lossy().to_string())
            .with("exe", std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default())),

        "llama_status" => {
            let mut s = super::llamacpp::status(core);
            if p.get("devices").bool_or(false) {
                s.set("devices", super::llamacpp::devices(core));
            }
            Ok(s)
        }

        "llama_install" => {
            let v = p.get("variant").str_or("cuda");
            let bin = super::llamacpp::install(core, &v)?;
            let mut s = super::llamacpp::status(core);
            s.set("installed", bin);
            s.set("devices", super::llamacpp::devices(core));
            Ok(s)
        }

        "llama_set_bin" => {
            let dir = p.get("dir").str_or("");
            let exe = std::path::Path::new(&dir).join(super::llamacpp::EXE);
            if !exe.is_file() {
                return Err(format!("В папке нет {} — выберите папку, где он лежит", super::llamacpp::EXE));
            }
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let mut l = st.get("llama").clone();
                if l.is_null() {
                    l = Json::obj();
                }
                l.set("bin_dir", dir.as_str());
                st.set("llama", l);
            }
            core.save_settings();
            let mut s = super::llamacpp::status(core);
            s.set("devices", super::llamacpp::devices(core));
            Ok(s)
        }

        "llama_scan" => {
            let dir = p.get("dir").str_or("");
            if !std::path::Path::new(&dir).is_dir() {
                return Err("Папка не найдена".into());
            }
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let mut l = st.get("llama").clone();
                if l.is_null() {
                    l = Json::obj();
                }
                l.set("models_dir", dir.as_str());
                st.set("llama", l);
            }
            core.save_settings();
            Ok(Json::obj().with("models", Json::Arr(super::llamacpp::scan_models(&dir))))
        }

        "llama_start" => {
            let models = p.get("models").str_list();
            let opts = super::llamacpp::StartOpts {
                ctx: p.get("ctx").i64_or(16384).clamp(2048, 1_048_576),
                gpu: p.get("gpu").str_or("gpu"),
                kv_q8: p.get("kv_q8").bool_or(true),
                extra: p.get("extra").str_or(""),
            };
            // запоминаем выбор
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let mut l = st.get("llama").clone();
                if l.is_null() {
                    l = Json::obj();
                }
                l.set("selected", models.clone());
                l.set("ctx", opts.ctx);
                l.set("gpu", opts.gpu.as_str());
                l.set("kv_q8", opts.kv_q8);
                l.set("extra", opts.extra.as_str());
                st.set("llama", l);
            }
            core.save_settings();
            core.cancel.store(false, Ordering::SeqCst);
            let started = super::llamacpp::start(core, &models, &opts)?;
            super::llamacpp::register(core, &started, opts.ctx);
            core.log(&format!("llama.cpp: запущено моделей — {}", started.len()));
            let mut st = core.state_json();
            st.set("llama", super::llamacpp::status(core));
            Ok(st)
        }

        // LM Studio и Ollama в один клик
        "local_status" => Ok(super::localsrv::status(core)),
        "local_install" => {
            let kind = p.get("kind").str_or("");
            let msg = super::localsrv::install(core, &kind)?;
            Ok(super::localsrv::status(core).with("message", msg))
        }
        "local_start" => {
            let kind = p.get("kind").str_or("");
            let msg = super::localsrv::start(core, &kind)?;
            super::localsrv::ensure_provider(core, &kind, None, false);
            Ok(super::localsrv::status(core).with("message", msg))
        }
        "local_stop" => {
            let kind = p.get("kind").str_or("");
            let msg = super::localsrv::stop(core, &kind)?;
            Ok(super::localsrv::status(core).with("message", msg))
        }
        "local_pull" => {
            let kind = p.get("kind").str_or("");
            let model = p.get("model").str_or("");
            let msg = super::localsrv::pull(core, &kind, &model)?;
            Ok(super::localsrv::status(core).with("message", msg))
        }
        "local_cancel" => Ok(Json::obj().with("cancelled", super::localsrv::cancel())),
        "local_use" => {
            // выбрать модель сервера основной нейросетью
            let kind = p.get("kind").str_or("");
            let model = p.get("model").str_or("");
            let id = super::localsrv::ensure_provider(core, &kind, if model.is_empty() { None } else { Some(model.as_str()) }, p.get("activate").bool_or(true));
            Ok(Json::obj().with("id", id).with("settings", core.settings().clone()))
        }

        "llama_stop" => {
            core.cancel.store(true, Ordering::SeqCst);
            let n = super::llamacpp::stop_all();
            core.cancel.store(false, Ordering::SeqCst);
            Ok(Json::obj().with("stopped", n))
        }

        "moodle_import" => {
            // архив «Скачать все ответы» (.zip) или уже распакованная папка
            let path = p.get("path").str_or("");
            if path.is_empty() {
                return Err("Выберите архив Moodle или папку".into());
            }
            let src = std::path::PathBuf::from(&path);
            let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "moodle".into());
            let root = if path.to_lowercase().ends_with(".zip") {
                let safe: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == ' ' || c == '_' { c } else { '_' }).take(60).collect();
                let dest = core.data_dir.join("moodle").join(format!("{}-{}", safe.trim(), super::core::now_s()));
                let n = super::moodle::unzip(&path, &dest)?;
                core.log(&format!("Moodle: распаковано файлов: {} → {}", n, dest.display()));
                dest
            } else if src.is_dir() {
                src.clone()
            } else {
                return Err("Нужен .zip-архив из Moodle или папка с распакованными ответами".into());
            };
            let (subs, skipped) = super::moodle::collect(&root, super::core::SUPPORTED);
            if subs.is_empty() {
                return Err("В архиве не найдено ответов Moodle: имена файлов должны быть вида «Фамилия Имя_123456_assignsubmission_file_…». Скачайте ответы кнопкой «Скачать все ответы» в задании Moodle.".into());
            }
            let group = {
                let g = p.get("group").str_or("");
                if g.trim().is_empty() { "Moodle".to_string() } else { g.trim().to_string() }
            };
            // тот же архив, загруженный повторно: ответы, которые уже есть, не дублируем
            // (но файлы, которых у такой работы ещё нет, — например Excel, — присоединяем к ней)
            let mut updated = 0usize;
            let subs: Vec<super::moodle::Submission> = {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let mut fresh = Vec::new();
                for s in subs {
                    let h = std::fs::read(&s.main).map(|b| super::core::fnv(&b)).unwrap_or(0);
                    let name = super::core::file_name(&s.main);
                    match ws.works.iter_mut().find(|w| w.student == s.student && w.hash == h && super::core::file_name(&w.path) == name) {
                        Some(w) => {
                            let have: Vec<String> = w.sources.iter().map(|x| super::core::file_name(x)).collect();
                            let missing: Vec<String> = s.extra.iter().filter(|x| !have.contains(&super::core::file_name(x))).cloned().collect();
                            if !missing.is_empty() {
                                core.attach_sources(w, &missing);
                                updated += 1;
                                let id = w.id.clone();
                                // работу с новыми файлами нужно проверить заново
                                if let Some(o) = ws.outcomes.get_mut(&id) {
                                    if o.status == "done" {
                                        o.review.insert(0, "Добавлены файлы студента из Moodle — проверьте работу заново".into());
                                    }
                                }
                            }
                        }
                        None => fresh.push(s),
                    }
                }
                fresh
            };
            if subs.is_empty() {
                core.compute_plagiarism();
                core.save_session();
                if updated == 0 {
                    return Err("Все ответы из этого архива уже загружены".into());
                }
                let mut st = core.state_json();
                st.set("moodle", Json::obj().with("students", 0).with("updated", updated).with("group", p.get("group").str_or("").as_str()).with("extra", 0).with("skipped", Json::Arr(vec![])).with("skipped_count", 0));
                return Ok(st);
            }
            let paths: Vec<String> = subs.iter().map(|s| s.main.clone()).collect();
            let errors = core.add_works_ex(&paths, None, &group, false);
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                for s in &subs {
                    if let Some(w) = ws.works.iter_mut().find(|w| w.path == s.main) {
                        w.student = s.student.clone();
                        // срок сдачи, указанный при импорте: время сдачи берётся из архива
                        let dl = p.get("deadline").f64_or(0.0);
                        if dl > 0.0 {
                            w.deadline = dl as u64;
                        }
                        // все остальные файлы студента — в ту же работу: Excel и PowerPoint
                        // (часть заданий), второй документ, PDF, фото страниц и скриншоты
                        if !s.extra.is_empty() {
                            core.attach_sources(w, &s.extra);
                        }
                    }
                }
            }
            // студентов — в список группы (журнал), если их там ещё нет
            if p.get("roster").bool_or(true) {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                let list = j.rosters.entry(group.clone()).or_default();
                let mut added = 0;
                for s in &subs {
                    if !list.iter().any(|n| super::moodle::same_person(n, &s.student)) {
                        list.push(s.student.clone());
                        added += 1;
                    }
                }
                if added > 0 {
                    j.save();
                }
            }
            core.save_session();
            let mut st = core.state_json();
            st.set("errors", errors);
            st.set(
                "moodle",
                Json::obj()
                    .with("students", subs.len())
                    .with("updated", updated)
                    .with("group", group.as_str())
                    .with("extra", subs.iter().map(|s| s.extra.len()).sum::<usize>())
                    .with("skipped", skipped.iter().take(20).cloned().collect::<Vec<_>>())
                    .with("skipped_count", skipped.len()),
            );
            Ok(st)
        }

        "moodle_import_grades" => {
            // оценки из Moodle → журнал группы
            let path = p.get("path").str_or("");
            let group = p.get("group").str_or("").trim().to_string();
            if group.is_empty() {
                return Err("Выберите группу".into());
            }
            // строки могут прийти готовыми (оценки, загруженные из Moodle напрямую)
            let rows: Vec<Vec<String>> = if !p.get("rows").as_arr().is_empty() {
                p.get("rows").as_arr().iter().map(|r| r.as_arr().iter().map(|c| c.str_or("")).collect()).collect()
            } else {
                let bytes = std::fs::read(&path).map_err(|e| format!("не удалось открыть файл: {}", e))?;
                super::moodle::table_rows(&path, &bytes)
            };
            let list = super::moodle::import_grades_as(&rows, &p.get("code").str_or(""), &p.get("prefer").str_or("auto"))?;
            let replace = p.get("replace").bool_or(false);
            let add_roster = p.get("roster").bool_or(true);
            let (mut added, mut updated, mut skipped, mut new_students, mut flagged) = (0, 0, 0, 0, 0);
            let (mut comments, mut no_variant) = (0, 0);
            // одинаковые ФИО в разных строках файла (например, обезличенный файл) — оценки таких строк сольются
            let mut seen_names: std::collections::BTreeMap<String, usize> = Default::default();
            {
                let mut per: std::collections::BTreeSet<(String, String)> = Default::default();
                for im in &list {
                    if !per.insert((super::moodle::name_key(&im.student), im.code.clone())) {
                        *seen_names.entry(super::moodle::name_key(&im.student)).or_default() += 1;
                    }
                }
            }
            let dup_rows: usize = seen_names.values().sum();
            let mut codes: Vec<String> = Vec::new();
            let mut tests: Vec<String> = Vec::new();
            let subject = p.get("subject").str_or("").trim().to_string();
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                // группа из импорта сразу видна в «Группах и направлениях»
                j.ensure_group(&group);
                for im in &list {
                    // ФИО из Moodle — к записи в списке группы («Лесовиков Иван» = «Иван Лесовиков»)
                    let roster = j.rosters.get(&group).cloned().unwrap_or_default();
                    let name = match roster.iter().find(|n| super::moodle::same_person(n, &im.student)) {
                        Some(n) => n.clone(),
                        None => {
                            if add_roster {
                                j.rosters.entry(group.clone()).or_default().push(im.student.clone());
                                new_students += 1;
                            }
                            im.student.clone()
                        }
                    };
                    if !im.email.trim().is_empty() {
                        j.emails.entry(group.clone()).or_default().insert(name.clone(), im.email.trim().to_string());
                    }
                    if !codes.contains(&im.code) {
                        codes.push(im.code.clone());
                        j.touch_topic(&group, &im.code, &im.kind, &im.title, &subject);
                        j.note_moodle(&group, &im.code, &im.header, p.get("scales").get(&im.header).as_f64().unwrap_or(im.scale));
                        if j.kind_of(&group, &im.code) == "test" {
                            tests.push(im.code.clone());
                        }
                    }
                    let exists = j.entries.iter().any(|e| e.group == group && e.student == name && e.code == im.code);
                    if exists && !replace {
                        // оценку не трогаем, но отзыв из Moodle дописываем, если его ещё не было
                        if !im.comment.trim().is_empty() {
                            if let Some(e) = j.entries.iter_mut().find(|e| e.group == group && e.student == name && e.code == im.code && e.comment.trim().is_empty()) {
                                e.comment = im.comment.clone();
                                e.flag = super::journal::comment_flags(&im.comment);
                                comments += 1;
                                if super::journal::mentions_ai(&im.comment) {
                                    flagged += 1;
                                }
                                if super::journal::mentions_no_variant(&im.comment) {
                                    no_variant += 1;
                                }
                                continue;
                            }
                        }
                        skipped += 1;
                        continue;
                    }
                    if !im.comment.trim().is_empty() {
                        comments += 1;
                    }
                    if super::journal::mentions_no_variant(&im.comment) {
                        no_variant += 1;
                    }
                    j.upsert(Entry {
                        group: group.clone(),
                        student: name,
                        code: im.code.clone(),
                        // как в Moodle, до сотых: тест на 2,6 или 3,8 так и остаётся (не округляется до половинок)
                        grade: Some(((im.grade.clamp(0.0, 5.0)) * 100.0).round() / 100.0),
                        violation: String::new(),
                        date: super::core::now_s(),
                        file: "из Moodle".into(),
                        variant: String::new(),
                        comment: im.comment.clone(),
                        style: Vec::new(),
                        // в отзыве преподаватель упомянул ИИ или нейросеть — помечаем оценку
                        flag: super::journal::comment_flags(&im.comment),
                    });
                    if super::journal::mentions_ai(&im.comment) {
                        flagged += 1;
                    }
                    if exists {
                        updated += 1;
                    } else {
                        added += 1;
                    }
                }
                // студенты без единой оценки в файле — тоже в список группы (у них долги)
                if add_roster {
                    for (name, email) in super::moodle::students_of(&rows) {
                        let roster = j.rosters.get(&group).cloned().unwrap_or_default();
                        if !roster.iter().any(|n| super::moodle::same_person(n, &name)) {
                            j.rosters.entry(group.clone()).or_default().push(name.clone());
                            new_students += 1;
                            if !email.trim().is_empty() {
                                j.emails.entry(group.clone()).or_default().insert(name, email.trim().to_string());
                            }
                        }
                    }
                }
                j.save();
            }
            let mut v = journal_view(core, &p);
            v.set("imported", Json::obj().with("added", added).with("updated", updated).with("skipped", skipped).with("students", new_students).with("codes", codes).with("tests", tests).with("flagged", flagged).with("comments", comments).with("no_variant", no_variant).with("dup_names", seen_names.len()).with("dup_rows", dup_rows).with("group", group.as_str()));
            Ok(v)
        }

        // ------------------------------------------------ Moodle напрямую (веб-службы)
        "moodle_ws_connect" => {
            // адрес + ключ, или адрес + логин/пароль (ключ службы мобильного приложения)
            let url = super::moodle_ws::normalize_base(&p.get("url").str_or(""));
            if url.len() < 12 {
                return Err("Укажите адрес сайта Moodle, например https://moodle.college.ru".into());
            }
            let mut token = p.get("token").str_or("").trim().to_string();
            if token.is_empty() {
                let user = p.get("username").str_or("");
                let pass = p.get("password").str_or("");
                if user.trim().is_empty() || pass.is_empty() {
                    return Err("Укажите ключ доступа или логин и пароль от Moodle".into());
                }
                token = super::moodle_ws::get_token(&*core.http, &url, user.trim(), &pass, &p.get("service").str_or(""))?;
            }
            let ws = super::moodle_ws::Ws::new(&url, &token);
            let info = ws.site_info(&*core.http)?;
            let conf = Json::obj()
                .with("url", url.as_str())
                .with("token", token.as_str())
                .with("site", info.get("sitename").str_or("").as_str())
                .with("user", info.get("fullname").str_or("").as_str())
                .with("userid", info.get("userid").f64_or(0.0));
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                st.set("moodle_ws", conf);
            }
            core.save_settings();
            core.log(&format!("Moodle: подключено к «{}» как {}", info.get("sitename").str_or(""), info.get("fullname").str_or("")));
            Ok(core.state_json())
        }

        "moodle_ws_disconnect" => {
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let url = st.get("moodle_ws").get("url").clone();
                st.set("moodle_ws", Json::obj().with("url", url).with("token", "").with("site", "").with("user", "").with("userid", 0));
            }
            core.save_settings();
            Ok(core.state_json())
        }

        "moodle_ws_courses" => {
            let (ws, uid) = moodle_ws_of(core)?;
            let list = ws.courses(&*core.http, uid)?;
            let courses: Vec<Json> = list.iter().map(|c| Json::obj().with("id", c.get("id").f64_or(0.0)).with("name", c.get("fullname").str_or("").as_str()).with("short", c.get("shortname").str_or("").as_str())).collect();
            Ok(Json::obj().with("courses", Json::Arr(courses)))
        }

        "moodle_ws_course" => {
            // задания и группы курса
            let (ws, _) = moodle_ws_of(core)?;
            let cid = p.get("course").f64_or(0.0) as i64;
            let assigns: Vec<Json> = ws
                .assignments(&*core.http, cid)?
                .iter()
                .map(|a| Json::obj().with("id", a.get("id").f64_or(0.0)).with("name", a.get("name").str_or("").as_str()).with("duedate", a.get("duedate").f64_or(0.0)).with("grade", a.get("grade").f64_or(0.0)))
                .collect();
            let groups: Vec<Json> = ws.groups(&*core.http, cid).unwrap_or_default().iter().map(|g| Json::obj().with("id", g.get("id").f64_or(0.0)).with("name", g.get("name").str_or("").as_str())).collect();
            Ok(Json::obj().with("assignments", Json::Arr(assigns)).with("groups", Json::Arr(groups)))
        }

        "moodle_ws_fetch" => {
            // ответы задания → папка как у архива «Скачать все ответы» → обычная загрузка из Moodle
            let (ws, _) = moodle_ws_of(core)?;
            let cid = p.get("course").f64_or(0.0) as i64;
            let aid = p.get("assign").f64_or(0.0) as i64;
            let mgroup = p.get("mgroup").f64_or(0.0) as i64;
            let users = ws.users(&*core.http, cid)?;
            let in_group = |u: &Json| mgroup == 0 || u.get("groups").as_arr().iter().any(|g| g.get("id").f64_or(0.0) as i64 == mgroup);
            let subs = ws.submissions(&*core.http, aid)?;
            let aname = p.get("assign_name").str_or("задание");
            let dest = core.data_dir.join("moodle").join(format!("{}-{}", super::moodle_ws::safe_name(&aname).chars().take(50).collect::<String>(), super::core::now_s()));
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            let (mut students, mut files, mut errors) = (0usize, 0usize, Vec::<String>::new());
            for s in subs.iter().filter(|s| s.status == "submitted" || !s.files.is_empty() || !s.text.trim().is_empty()) {
                let Some(u) = users.iter().find(|u| u.get("id").f64_or(0.0) as i64 == s.userid) else { continue };
                if !in_group(u) {
                    continue;
                }
                let full = u.get("fullname").str_or("");
                let base = format!("{}_{}_assignsubmission_file_", super::moodle_ws::safe_name(&full), s.userid);
                let mut any = false;
                for f in &s.files {
                    let dir = dest.join(&base);
                    let _ = std::fs::create_dir_all(&dir);
                    match ws.download(&*core.http, &f.url) {
                        Ok(b) => {
                            let fp = dir.join(super::moodle_ws::safe_name(&f.name));
                            if std::fs::write(&fp, &b).is_ok() {
                                // время сдачи — как в Moodle (по нему считается опоздание)
                                let t = if f.mtime > 0 { f.mtime } else { s.modified };
                                if t > 0 {
                                    if let Ok(fh) = std::fs::File::options().write(true).open(&fp) {
                                        let _ = fh.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t));
                                    }
                                }
                                files += 1;
                                any = true;
                            }
                        }
                        Err(e) => errors.push(format!("{} — {}: {}", full, f.name, e)),
                    }
                }
                if !s.text.trim().is_empty() {
                    let fp = dest.join(format!("{}_{}_assignsubmission_onlinetext_onlinetext.html", super::moodle_ws::safe_name(&full), s.userid));
                    if std::fs::write(&fp, format!("<html><body>{}</body></html>", s.text)).is_ok() {
                        files += 1;
                        any = true;
                    }
                }
                if any {
                    students += 1;
                }
            }
            if students == 0 {
                let _ = std::fs::remove_dir_all(&dest);
                return Err(if errors.is_empty() { "В этом задании пока нет ответов студентов (с учётом выбранной группы)".into() } else { format!("Не удалось скачать ответы: {}", errors.join("; ")) });
            }
            core.log(&format!("Moodle: скачано ответов {} (файлов {}) из «{}»", students, files, aname));
            let mut p2 = p.clone();
            p2.set("path", dest.to_string_lossy().to_string());
            let mut st = handle_inner(core, "moodle_import", &p2)?;
            st.set("fetched", Json::obj().with("students", students).with("files", files).with("errors", errors));
            Ok(st)
        }

        "moodle_ws_grades" => {
            // оценки и отзывы курса из Moodle → тот же разбор, что у файла экспорта
            let (ws, _) = moodle_ws_of(core)?;
            let cid = p.get("course").f64_or(0.0) as i64;
            let mgroup = p.get("mgroup").f64_or(0.0) as i64;
            let only: Option<Vec<i64>> = if mgroup > 0 {
                Some(ws.users(&*core.http, cid)?.iter().filter(|u| u.get("groups").as_arr().iter().any(|g| g.get("id").f64_or(0.0) as i64 == mgroup)).map(|u| u.get("id").f64_or(0.0) as i64).collect())
            } else {
                None
            };
            let (rows, scales) = ws.grade_rows(&*core.http, cid, only.as_deref())?;
            if rows.len() < 2 {
                return Err("В курсе нет студентов с оценками (или у ключа нет доступа к журналу оценок)".into());
            }
            let mut p2 = p.clone();
            p2.set("rows", Json::Arr(rows.into_iter().map(|r| Json::Arr(r.into_iter().map(Json::from).collect())).collect()));
            p2.set("prefer", "value");
            let mut sc = Json::obj();
            for (h, m) in scales {
                sc.set(&h, m);
            }
            p2.set("scales", sc);
            handle_inner(core, "moodle_import_grades", &p2)
        }

        "moodle_ws_push" => {
            // оценки (и отзывы) журнала группы → задания Moodle
            let (ws, _) = moodle_ws_of(core)?;
            let cid = p.get("course").f64_or(0.0) as i64;
            let group = p.get("group").str_or("");
            let with_fb = p.get("feedback").bool_or(false);
            let dry = p.get("dry").bool_or(false);
            let assigns = ws.assignments(&*core.http, cid)?;
            let users = ws.users(&*core.http, cid)?;
            let j = core.journal.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut codes = p.get("codes").str_list();
            if codes.is_empty() {
                codes = j.codes_of(&group).into_iter().filter(|c| j.kind_of(&group, c) == "work").collect();
            }
            // задание Moodle для темы: по заголовку из Moodle, полному названию или шифру
            let norm = |s: &str| s.to_lowercase().replace('ё', "е").split_whitespace().collect::<Vec<_>>().join(" ");
            let mut plan: Vec<Json> = Vec::new();
            let (mut sent, mut failed, mut no_user) = (0usize, Vec::<String>::new(), Vec::<String>::new());
            for c in &codes {
                let t = j.topic(&group, c);
                let mname = t.moodle.splitn(2, ':').nth(1).map(|x| x.trim().to_string()).unwrap_or_default();
                let manual = p.get("map").get(c).f64_or(0.0) as i64;
                let a = assigns.iter().find(|a| manual > 0 && a.get("id").f64_or(0.0) as i64 == manual).or_else(|| {
                    assigns.iter().find(|a| {
                        let n = norm(&a.get("name").str_or(""));
                        (!mname.is_empty() && n == norm(&mname)) || (!t.title.is_empty() && n == norm(&t.title)) || n == norm(c) || super::text::find_work_code(&a.get("name").str_or("")).map(|x| x == *c).unwrap_or(false)
                    })
                });
                let Some(a) = a else {
                    plan.push(Json::obj().with("code", c.as_str()).with("assign", Json::Null).with("n", 0));
                    continue;
                };
                let aid = a.get("id").f64_or(0.0) as i64;
                let max = a.get("grade").f64_or(0.0);
                let entries: Vec<&super::journal::Entry> = j.entries.iter().filter(|e| e.group == group && &e.code == c && e.grade.is_some()).collect();
                plan.push(Json::obj().with("code", c.as_str()).with("assign", a.get("name").str_or("").as_str()).with("assign_id", aid).with("n", entries.len()).with("scale", max <= 0.0));
                if dry || max <= 0.0 {
                    continue;
                }
                let emails = j.emails.get(&group).cloned().unwrap_or_default();
                for e in entries {
                    let mail = emails.get(&e.student).cloned().unwrap_or_default().to_lowercase();
                    let u = users.iter().find(|u| (!mail.is_empty() && u.get("email").str_or("").to_lowercase() == mail) || super::moodle::same_person(&u.get("fullname").str_or(""), &e.student) || super::moodle::same_person(&format!("{} {}", u.get("lastname").str_or(""), u.get("firstname").str_or("")), &e.student));
                    let Some(u) = u else {
                        if !no_user.contains(&e.student) {
                            no_user.push(e.student.clone());
                        }
                        continue;
                    };
                    let g = e.grade.unwrap_or(0.0);
                    let val = if (max - 5.0).abs() < 1e-9 { g } else { g / 5.0 * max };
                    let fb = if with_fb && !e.comment.trim().is_empty() { Some(e.comment.as_str()) } else { None };
                    match ws.save_grade(&*core.http, aid, u.get("id").f64_or(0.0) as i64, val, fb) {
                        Ok(()) => sent += 1,
                        Err(er) => failed.push(format!("{} ({}): {}", e.student, c, er)),
                    }
                }
            }
            if !dry {
                core.log(&format!("Moodle: отправлено оценок {}, ошибок {}", sent, failed.len()));
            }
            Ok(Json::obj().with("plan", Json::Arr(plan)).with("sent", sent).with("failed", failed).with("no_user", no_user).with("assignments", Json::Arr(assigns.iter().map(|a| Json::obj().with("id", a.get("id").f64_or(0.0)).with("name", a.get("name").str_or("").as_str())).collect())))
        }

        "moodle_export_gradebook" => {
            // журнал группы → CSV для «Оценки → Импорт» в Moodle
            let path = p.get("path").str_or("");
            let group = p.get("group").str_or("");
            let j = core.journal.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut codes: Vec<String> = p.get("codes").str_list();
            if codes.is_empty() {
                codes = j.entries.iter().filter(|e| e.group == group).map(|e| e.code.clone()).collect();
                codes.sort_by_key(|c| natural_key(c));
                codes.dedup();
            }
            if codes.is_empty() {
                return Err("В журнале этой группы нет оценок".into());
            }
            let mut students: Vec<String> = j.rosters.get(&group).cloned().unwrap_or_default();
            for e in j.entries.iter().filter(|e| e.group == group) {
                if !students.contains(&e.student) {
                    students.push(e.student.clone());
                }
            }
            let emails = j.emails.get(&group).cloned().unwrap_or_default();
            let list: Vec<(String, String)> = students.iter().map(|s| (s.clone(), emails.get(s).cloned().unwrap_or_default())).collect();
            let no_mail = list.iter().filter(|(s, m)| m.is_empty() && j.entries.iter().any(|e| e.group == group && &e.student == s)).count();
            // свои столбцы (пропуски, посещения) в Moodle не выгружаем
            codes.retain(|c| j.kind_of(&group, c) != "custom");
            let with_fb = p.get("feedback").bool_or(false);
            let sep = p.get("sep").str_or(";").chars().next().unwrap_or(';');
            let fb = |s: &str, c: &str| j.entries.iter().find(|e| e.group == group && e.student == s && e.code == c).map(|e| e.comment.clone()).unwrap_or_default();
            // под именами элементов Moodle (если тема пришла из Moodle), иначе — под шифрами
            let use_names = p.get("moodle_names").bool_or(true);
            let items: Vec<(String, String, f64)> = codes
                .iter()
                .map(|c| {
                    let t = j.topic(&group, c);
                    let sc = t.mscale.replace(',', ".").parse::<f64>().unwrap_or(5.0);
                    if use_names { (c.clone(), t.moodle.clone(), sc) } else { (c.clone(), String::new(), 5.0) }
                })
                .collect();
            let csv = super::moodle::export_gradebook_named(&list, &items, &|s, c| j.entries.iter().find(|e| e.group == group && e.student == s && e.code == c).and_then(|e| e.grade), if with_fb { Some(&fb) } else { None }, sep);
            let bytes = super::moodle::encode_csv(&csv, &p.get("encoding").str_or("utf8bom"));
            std::fs::write(&path, bytes).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path).with("codes", codes).with("no_email", no_mail))
        }

        "moodle_grades" => {
            // заполнить таблицу оценок Moodle: «Оценка» и «Отзыв в виде комментария»
            let path = p.get("path").str_or("");
            let text = std::fs::read(&path).map(|b| super::text::decode_bytes(&b)).map_err(|e| format!("не удалось открыть таблицу: {}", e))?;
            let (mut rows, sep) = super::moodle::parse_csv(&text);
            if rows.len() < 2 {
                return Err("Таблица пустая: скачайте её в Moodle — «Просмотр всех ответов» → «Действия с оценками» → «Скачать таблицу оценок»".into());
            }
            let head = rows[0].clone();
            let c_name = super::moodle::col(&head, &["полное имя", "full name", "фамилия и имя"]).ok_or("В таблице нет столбца «Полное имя»")?;
            let c_grade = super::moodle::col(&head, &["оценка", "grade"]).ok_or("В таблице нет столбца «Оценка»")?;
            let c_max = super::moodle::col(&head, &["максимальная оценка", "maximum grade"]);
            let c_fb = super::moodle::col(&head, &["отзыв в виде комментария", "feedback comments"]);
            let with_fb = p.get("feedback").bool_or(true);
            let only = p.get("group").str_or("");
            // оценки по студентам: из текущей проверки или из журнала группы (по коду работы)
            let mut graded: Vec<(String, f64, String)> = Vec::new();
            if p.get("source").str_or("") == "journal" {
                let code = p.get("code").str_or("");
                let j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                for e in j.entries.iter().filter(|e| (only.is_empty() || e.group == only) && e.code == code) {
                    if let Some(g) = e.grade {
                        graded.push((e.student.clone(), g, e.comment.clone()));
                    }
                }
            } else {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                for w in &ws.works {
                    if !only.is_empty() && w.group != only && core.program_of(&w.group) != only {
                        continue;
                    }
                    let Some(o) = ws.outcomes.get(&w.id) else { continue };
                    let Some(g) = o.final_grade() else { continue };
                    // работа в паре — оценка каждому напарнику
                    for student in core.authors(&ws, w).0 {
                        graded.push((student, g, o.feedback.clone()));
                    }
                }
            }
            let (mut hit, mut miss) = (0usize, Vec::new());
            let mut used = vec![false; graded.len()];
            for r in rows.iter_mut().skip(1) {
                while r.len() < head.len() {
                    r.push(String::new());
                }
                let name = r[c_name].clone();
                let Some(k) = graded.iter().enumerate().position(|(i, (s, _, _))| !used[i] && super::moodle::same_person(s, &name)) else { continue };
                used[k] = true;
                let (_, g, fb) = &graded[k];
                let max = c_max.and_then(|c| r[c].replace(',', ".").trim().parse::<f64>().ok()).filter(|m| *m > 0.0).unwrap_or(5.0);
                // шкала Moodle может быть не пятибалльной: переводим пропорционально
                let v = if (max - 5.0).abs() < 0.01 { *g } else { (*g / 5.0 * max * 100.0).round() / 100.0 };
                r[c_grade] = if (v - v.round()).abs() < 0.001 { format!("{}", v.round() as i64) } else { format!("{}", v) };
                if let (Some(c), true) = (c_fb, with_fb) {
                    if !fb.trim().is_empty() {
                        r[c] = fb.trim().to_string();
                    }
                }
                hit += 1;
            }
            for (i, (s, _, _)) in graded.iter().enumerate() {
                if !used[i] {
                    miss.push(s.clone());
                }
            }
            let src = std::path::Path::new(&path);
            let out = src.with_file_name(format!("{}_оценки.csv", src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Оценки".into())));
            std::fs::write(&out, super::moodle::encode_csv(&super::moodle::write_csv(&rows, sep), &p.get("encoding").str_or("utf8bom"))).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", out.to_string_lossy().to_string()).with("filled", hit).with("not_found", miss))
        }

        "export_feedback" => {
            let path = p.get("path").str_or("");
            let only = p.get("ids").str_list();
            let ids: Vec<String> = {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                ws.works
                    .iter()
                    .filter(|w| only.is_empty() || only.contains(&w.id))
                    .filter(|w| ws.outcomes.get(&w.id).and_then(|o| o.final_grade()).is_some())
                    .map(|w| w.id.clone())
                    .collect()
            };
            if ids.is_empty() {
                return Err("Нет проверенных работ для отзывов".into());
            }
            let mut blocks = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                let (title, text) = {
                    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    let w = ws.works.iter().find(|w| &w.id == id).unwrap();
                    let (student, _) = core.student_of(&ws, w);
                    let saved = ws.outcomes.get(id).map(|o| o.feedback.clone()).unwrap_or_default();
                    (format!("{} — {}{}", student, w.name, if w.group.is_empty() { String::new() } else { format!(", {}", w.group) }), saved)
                };
                let text = if text.trim().is_empty() { core.feedback_template(id).unwrap_or_default() } else { text };
                if i > 0 {
                    blocks.push(Block::PageBreak);
                }
                blocks.push(Block::Heading(title));
                blocks.push(Block::Para(text));
            }
            std::fs::write(&path, super::report::docx_blocks("Отзывы студентам", &blocks, false)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path).with("count", ids.len()))
        }

        "export_oral" => {
            let id = p.get("id").str_or("");
            let path = p.get("path").str_or("");
            let (title, oral) = {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let w = ws.works.iter().find(|w| w.id == id).ok_or("работа не найдена")?;
                let (student, _) = core.student_of(&ws, w);
                (format!("{} — {}", student, w.name), ws.outcomes.get(&id).map(|o| o.oral.clone()).unwrap_or_default())
            };
            let qs = oral.get("questions").as_arr().to_vec();
            if qs.is_empty() {
                return Err("Сначала составьте вопросы".into());
            }
            let mut blocks = vec![Block::Title("Вопросы для устной защиты".into()), Block::Sub(title)];
            for (i, q) in qs.iter().enumerate() {
                blocks.push(Block::Bold(format!("{}. {}", i + 1, q.get("q").str_or(""))));
                blocks.push(Block::Para(format!("Ожидаемый ответ: {}", q.get("expect").str_or(""))));
                if !q.get("why").str_or("").is_empty() {
                    blocks.push(Block::Para(format!("Что проверяет: {}", q.get("why").str_or(""))));
                }
            }
            blocks.push(Block::Para(format!("\nИтог защиты: ______________________      Дата: {}", super::report::today_str())));
            std::fs::write(&path, super::report::docx_blocks("Вопросы для защиты", &blocks, false)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path))
        }

        // ---------------------------------------------------------- журнал группы
        "journal" => Ok(journal_view(core, &p)),

        "journal_add" => {
            let n = core.journal_add_current();
            Ok(journal_view(core, &p).with("added", n))
        }

        "journal_roster" => {
            let group = p.get("group").str_or("").trim().to_string();
            if group.is_empty() {
                return Err("Укажите группу".into());
            }
            let names: Vec<String> = p
                .get("names")
                .str_list()
                .into_iter()
                .map(|n| {
                    // «1. Лесовиков Иван» → «Лесовиков Иван»
                    n.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ' || c == '\t').trim().to_string()
                })
                .filter(|n| !n.is_empty())
                .collect();
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                if names.is_empty() {
                    j.rosters.remove(&group);
                    j.assigned.remove(&group);
                    j.teams.remove(&group);
                } else {
                    j.rosters.insert(group.clone(), names);
                    j.ensure_group(&group);
                }
                if let Some(v) = p.get("variant_is_number").as_bool() {
                    j.variant_is_number = v;
                }
                // варианты и пары студентов: {ФИО: вариант}, {ФИО: номер пары}
                let names = j.rosters.get(&group).cloned().unwrap_or_default();
                for key in ["variants", "teams"] {
                    if p.get(key).is_null() {
                        continue;
                    }
                    let map: std::collections::BTreeMap<String, String> = p
                        .get(key)
                        .as_obj()
                        .iter()
                        .map(|(k, v)| (k.trim().to_string(), v.as_str().map(|x| x.to_string()).or_else(|| v.as_f64().map(|n| format!("{}", n as i64))).unwrap_or_default().trim().to_string()))
                        .filter(|(k, v)| !v.is_empty() && names.contains(k))
                        .collect();
                    let target = if key == "variants" { &mut j.assigned } else { &mut j.teams };
                    if map.is_empty() {
                        target.remove(&group);
                    } else {
                        target.insert(group.clone(), map);
                    }
                }
                j.save();
            }
            core.refinalize_all();
            Ok(journal_view(core, &p))
        }

        "analytics" => {
            // сводка по заданию: какие пункты решают неверно, варианты, частые ошибки, ИИ
            use std::collections::BTreeMap;
            let group = p.get("group").str_or("");
            let only = p.get("assignment").str_or("");
            #[derive(Default)]
            struct Acc {
                code: String,
                title: String,
                n: i64,
                sum: f64,
                dist: [i64; 4],
                ai: i64,
                plag: i64,
                tasks: BTreeMap<String, [i64; 4]>,
                variants: BTreeMap<String, (i64, f64)>,
                groups: BTreeMap<String, (i64, f64, i64)>,
                errors: BTreeMap<String, (i64, String)>,
                signs: BTreeMap<String, i64>,
                pending: i64,
            }
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
            for w in &ws.works {
                if !group.is_empty() && w.group != group {
                    continue;
                }
                let Some(set) = core.set_for_work(&ws, w) else { continue };
                if !only.is_empty() && set.id != only {
                    continue;
                }
                let a = acc.entry(set.id.clone()).or_default();
                a.code = set.code.clone();
                a.title = set.title.clone();
                let Some(o) = ws.outcomes.get(&w.id).filter(|o| o.status == "done" && !o.llm.is_null()) else {
                    a.pending += 1;
                    continue;
                };
                let Some(g) = o.final_grade() else { continue };
                a.n += 1;
                a.sum += g;
                a.dist[(g.floor().clamp(2.0, 5.0) as usize) - 2] += 1;
                if o.violation == "ai" {
                    a.ai += 1;
                }
                if o.violation == "plagiarism" {
                    a.plag += 1;
                }
                let gr = a.groups.entry(if w.group.is_empty() { "без группы".into() } else { w.group.clone() }).or_default();
                gr.0 += 1;
                gr.1 += g;
                if o.violation == "ai" {
                    gr.2 += 1;
                }
                let scan = core.scan(&ws, w);
                if let Some(v) = scan.variant_shown() {
                    let e = a.variants.entry(v).or_default();
                    e.0 += 1;
                    e.1 += g;
                }
                for t in o.llm.get("tasks").as_arr() {
                    let n = t.get("n").as_str().map(|x| x.to_string()).or_else(|| t.get("n").as_f64().map(|x| format!("{}", x))).unwrap_or_default();
                    if n.trim().is_empty() {
                        continue;
                    }
                    let k = match t.get("status").str_or("").to_lowercase().as_str() {
                        "верно" => 0,
                        "частично" => 2,
                        "не выполнено" => 3,
                        _ => 1,
                    };
                    a.tasks.entry(n.trim().to_string()).or_default()[k] += 1;
                }
                for e in o.llm.get("errors").str_list() {
                    // одна и та же ошибка у разных студентов: без цитат, по первым словам
                    let mut clean = String::new();
                    let mut depth = 0;
                    for c in e.chars() {
                        match c {
                            '«' => depth += 1,
                            '»' => depth = (depth - 1).max(0),
                            _ if depth == 0 => clean.push(c),
                            _ => {}
                        }
                    }
                    let key: String = clean.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|x| x.len() > 1).take(6).collect::<Vec<_>>().join(" ");
                    if key.is_empty() {
                        continue;
                    }
                    let x = a.errors.entry(key).or_insert((0, clean.trim().trim_end_matches(|c: char| c == ' ' || c == ',' || c == ':' || c == '—').to_string()));
                    x.0 += 1;
                }
                for e in scan.evidence.iter().filter(|e| e.weight != "weak") {
                    let what: String = e.what.split(':').next().unwrap_or("").chars().take(90).collect();
                    *a.signs.entry(what).or_default() += 1;
                }
            }
            let mut out = Json::arr();
            for (id, a) in acc {
                let mut tasks: Vec<(String, [i64; 4])> = a.tasks.into_iter().collect();
                tasks.sort_by_key(|(n, _)| natural_key(n));
                let mut errors: Vec<(String, (i64, String))> = a.errors.into_iter().collect();
                errors.sort_by(|x, y| y.1 .0.cmp(&x.1 .0));
                let mut signs: Vec<(String, i64)> = a.signs.into_iter().collect();
                signs.sort_by(|x, y| y.1.cmp(&x.1));
                let mut vars: Vec<(String, (i64, f64))> = a.variants.into_iter().collect();
                vars.sort_by_key(|(v, _)| natural_key(v));
                out.push(
                    Json::obj()
                        .with("id", id.as_str())
                        .with("code", a.code.as_str())
                        .with("title", a.title.as_str())
                        .with("n", a.n)
                        .with("pending", a.pending)
                        .with("avg", if a.n > 0 { Json::Num((a.sum / a.n as f64 * 100.0).round() / 100.0) } else { Json::Null })
                        .with("dist", Json::Arr(a.dist.iter().map(|x| Json::from(*x)).collect()))
                        .with("ai", a.ai)
                        .with("plag", a.plag)
                        .with("tasks", Json::Arr(tasks.into_iter().map(|(n, c)| Json::obj().with("n", n).with("ok", c[0]).with("errors", c[1]).with("partial", c[2]).with("missing", c[3])).collect()))
                        .with("variants", Json::Arr(vars.into_iter().map(|(v, (n, s))| Json::obj().with("variant", v).with("n", n).with("avg", (s / n as f64 * 100.0).round() / 100.0)).collect()))
                        .with("groups", Json::Arr(a.groups.into_iter().map(|(g, (n, s, ai))| Json::obj().with("group", g).with("n", n).with("avg", (s / n as f64 * 100.0).round() / 100.0).with("ai", ai)).collect()))
                        .with("errors", Json::Arr(errors.into_iter().take(10).map(|(_, (n, t))| Json::obj().with("text", t).with("n", n)).collect()))
                        .with("signs", Json::Arr(signs.into_iter().take(8).map(|(t, n)| Json::obj().with("text", t).with("n", n)).collect())),
                );
            }
            Ok(Json::obj().with("sets", out))
        }

        "set_deadline" => {
            // срок сдачи для выбранных работ (0 — убрать); правило «позже срока» пересчитывается
            let ids = p.get("ids").str_list();
            let at = p.get("deadline").f64_or(0.0).max(0.0) as u64;
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                for w in ws.works.iter_mut().filter(|w| ids.contains(&w.id)) {
                    w.deadline = at;
                }
            }
            for id in &ids {
                let _ = core.refinalize(id);
            }
            core.save_session();
            Ok(core.state_json())
        }

        // ---- задания с фото, эталоны нейросетью, вопросы перед генерацией
        "read_path_b64" => {
            let path = p.get("path").str_or("");
            if !super::docx::is_image(&path) {
                return Err("Это не изображение".into());
            }
            let b = std::fs::read(&path).map_err(|e| e.to_string())?;
            Ok(Json::obj().with("b64", b64(&b)).with("mime", super::docx::image_mime(&path)).with("name", super::core::file_name(&path).as_str()))
        }
        "photo_text" => {
            let images: Vec<(String, String)> = p.get("images").as_arr().iter().map(|i| (i.get("mime").str_or("image/jpeg"), i.get("b64").str_or(""))).filter(|(_, b)| !b.is_empty()).collect();
            let (task, key) = super::taskphoto::photo_text(core, &images, &p.get("purpose").str_or("task"), &p.get("subject").str_or(""))?;
            Ok(Json::obj().with("task", task).with("key", key))
        }
        "solve_task" => {
            let t = super::taskphoto::solve(core, &p.get("task").str_or(""), &p.get("subject").str_or(""), &p.get("level").str_or(""))?;
            Ok(Json::obj().with("key", t))
        }
        "save_task_text" => {
            let (tp, kp) = super::taskphoto::save(core, &p.get("code").str_or(""), &p.get("title").str_or(""), &p.get("scope").str_or(""), &p.get("task").str_or(""), &p.get("key").str_or(""))?;
            Ok(core.state_json().with("task_path", tp).with("key_path", kp))
        }
        "set_text" => {
            // условие и ответы набора одним текстом (для «Нет ответов» и генератора)
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let set = ws.sets.iter().find(|s| s.id == p.get("set").str_or("")).ok_or("задание не найдено")?;
            let many = set.variants.len() > 1;
            let mut task = String::new();
            let mut key = String::new();
            for (id, v) in &set.variants {
                if many && id != "*" {
                    task.push_str(&format!("Вариант {}\n", id));
                }
                task.push_str(v.task_visible.trim());
                task.push_str("\n\n");
                if !v.key.trim().is_empty() {
                    if many && id != "*" {
                        key.push_str(&format!("Вариант {}\n", id));
                    }
                    key.push_str(v.key.trim());
                    key.push_str("\n\n");
                }
            }
            Ok(Json::obj().with("task", task.trim()).with("key", key.trim()).with("code", set.code.as_str()).with("title", set.title.as_str()).with("scope", set.scope.as_str()))
        }
        "gen_plan" => {
            let (sample, key) = if !p.get("sample").str_or("").trim().is_empty() {
                (p.get("sample").str_or(""), p.get("key").str_or(""))
            } else {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let set = ws.sets.iter().find(|s| s.id == p.get("set").str_or("")).ok_or("Выберите задание-образец")?;
                let v = set.variant(&p.get("variant").str_or("")).or_else(|| set.variants.values().next()).ok_or("В задании нет вариантов")?;
                (v.task_visible.clone(), v.key.clone())
            };
            super::taskphoto::plan(core, &sample, &key)
        }

        "gen_variants" => {
            // новые варианты задания нейросетью: файл заданий со скрытыми ловушками + файл ответов
            let own = !p.get("sample").str_or("").trim().is_empty();
            let (sample, key, title, code, next) = if own {
                // образец с фото или вписанный вручную
                let code = p.get("code").str_or("").trim().to_string();
                (p.get("sample").str_or(""), p.get("key").str_or(""), p.get("title").str_or(&code), if code.is_empty() { "Задание".to_string() } else { code }, 1usize)
            } else {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let set = ws.sets.iter().find(|s| s.id == p.get("set").str_or("")).ok_or("Выберите задание-образец")?;
                let base = p.get("variant").str_or("");
                let v = set.variant(&base).or_else(|| set.variants.values().next()).ok_or("В задании нет вариантов")?;
                let next = set.variants.keys().filter_map(|k| k.parse::<usize>().ok()).max().unwrap_or(0) + 1;
                (v.task_visible.clone(), v.key.clone(), if set.title.is_empty() { set.code.clone() } else { set.title.clone() }, set.code.clone(), next)
            };
            let _ = own;
            let count = p.get("count").i64_or(5).clamp(1, 40) as usize;
            let start = p.get("start").i64_or(next as i64).max(1) as usize;
            let dir = p.get("dir").str_or("");
            if dir.is_empty() || !std::path::Path::new(&dir).is_dir() {
                return Err("Выберите папку, куда сохранить варианты".into());
            }
            let vars = super::variants::generate(core, &sample, &key, start, count, p.get("traps").bool_or(true), &p.get("wishes").str_or(""))?;
            let range = if count == 1 { format!("{}", start) } else { format!("{}-{}", start, start + count - 1) };
            let safe = |s: &str| s.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect::<String>();
            let tp = std::path::Path::new(&dir).join(format!("{}_варианты_{}.docx", safe(&code), range));
            let kp = std::path::Path::new(&dir).join(format!("{}_ответы_{}.docx", safe(&code), range));
            std::fs::write(&tp, super::variants::tasks_docx(&title, &vars)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            std::fs::write(&kp, super::variants::answers_docx(&title, &vars)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj()
                .with("tasks", tp.to_string_lossy().to_string())
                .with("answers", kp.to_string_lossy().to_string())
                .with("count", vars.len())
                .with("traps", vars.iter().map(|v| v.traps.len()).sum::<usize>())
                .with("preview", Json::Arr(vars.iter().take(2).map(|v| Json::obj().with("n", v.n).with("tasks", Json::Arr(v.tasks.iter().map(|(n, t)| Json::Str(format!("{}. {}", n, t))).collect())).with("traps", Json::Arr(v.traps.iter().map(|(a, h)| Json::Str(format!("после задания {}: {}", a, h))).collect()))).collect())))
        }

        "compare_works" => {
            // две работы рядом: совпадающие куски подсвечены, текст условия — отдельно
            let (a, b) = (p.get("a").str_or(""), p.get("b").str_or(""));
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let wa = ws.works.iter().find(|w| w.id == a).ok_or("работа не найдена")?;
            let wb = ws.works.iter().find(|w| w.id == b).ok_or("работа не найдена")?;
            let task: String = [core.set_for_work(&ws, wa), core.set_for_work(&ws, wb)]
                .iter()
                .flatten()
                .flat_map(|s| s.variants.values().map(|v| v.task_visible.clone()))
                .collect::<Vec<_>>()
                .join("\n");
            let (sa, sb, share_a) = super::detect::compare_marks(&wa.visible, &wb.visible, &task);
            let (_, _, share_b) = super::detect::compare_marks(&wb.visible, &wa.visible, &task);
            let seg = |v: Vec<(String, u8)>| Json::Arr(v.into_iter().map(|(t, k)| Json::Arr(vec![Json::Str(t), Json::from(k as i64)])).collect());
            let side = |w: &super::core::Work, segs: Json, share: f64| {
                let (student, _) = core.student_of(&ws, w);
                Json::obj().with("id", w.id.as_str()).with("name", w.name.as_str()).with("group", w.group.as_str()).with("student", student).with("segments", segs).with("share", (share * 100.0).round())
            };
            let score = ws.plag.iter().find(|(x, y, _)| (x == &a && y == &b) || (x == &b && y == &a)).map(|x| (x.2 * 100.0).round());
            Ok(Json::obj().with("a", side(wa, seg(sa), share_a)).with("b", side(wb, seg(sb), share_b)).with("score", score))
        }

        "roster_import" => {
            // список студентов из файла (Excel, Word, текст) или с фото / из буфера обмена
            let (text, from) = if let Some(img) = p.get("image").as_str().filter(|x| !x.is_empty()) {
                let mime = p.get("mime").str_or("image/png");
                (grader::vision_text(core, super::roster::SYSTEM_VISION_ROSTER, "Перепиши список студентов с этого изображения.", &[(mime, img.to_string())])?, "фото".to_string())
            } else {
                let path = p.get("path").str_or("");
                if path.is_empty() {
                    return Err("Выберите файл со списком".into());
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("Не удалось открыть файл: {}", e))?;
                if super::docx::is_image(&path) {
                    let mime = super::docx::image_mime(&path).to_string();
                    (grader::vision_text(core, super::roster::SYSTEM_VISION_ROSTER, "Перепиши список студентов с этого изображения.", &[(mime, b64(&bytes))])?, "фото".to_string())
                } else {
                    let ex = super::docx::extract(&path, &bytes);
                    if ex.needs_pdf {
                        return Err("Список в PDF не читается напрямую — сохраните его как Word или Excel, либо сделайте снимок экрана и загрузите как фото".into());
                    }
                    let t = ex.visible();
                    if t.trim().is_empty() {
                        return Err(ex.notes.first().cloned().unwrap_or_else(|| "В файле не найден текст".into()));
                    }
                    (t, super::core::file_name(&path))
                }
            };
            let rows = super::roster::parse(&text);
            if rows.is_empty() {
                return Err(format!("В {} не найдено ни одного ФИО. Проверьте, что в списке есть фамилии и имена (по одному студенту в строке или в ячейке).", if from == "фото" { "распознанном тексте" } else { "файле" }));
            }
            Ok(Json::obj()
                .with("rows", Json::Arr(rows.iter().map(|r| Json::obj().with("name", r.name.as_str()).with("variant", r.variant.clone().unwrap_or_default())).collect()))
                .with("from", from)
                .with("text", super::text::clip(&text, 4000)))
        }

        "roster_parse" => {
            let rows = super::roster::parse(&p.get("text").str_or(""));
            if rows.is_empty() {
                return Err("Не найдено ни одного ФИО — пишите по одному студенту в строке: «Фамилия Имя» (можно с вариантом: «Лесовиков Иван — 3»)".into());
            }
            Ok(Json::obj().with("rows", Json::Arr(rows.iter().map(|r| Json::obj().with("name", r.name.as_str()).with("variant", r.variant.clone().unwrap_or_default())).collect())))
        }

        "add_photo_work" => {
            // одна работа из нескольких фото (страницы тетради), студент выбран вручную
            let paths = p.get("paths").str_list();
            let group = p.get("group").str_or("").trim().to_string();
            let student = p.get("student").str_or("").trim().to_string();
            if paths.is_empty() {
                return Err("Выберите фото страниц работы".into());
            }
            let errors = core.add_works_ex(&paths[..1], None, &group, false);
            if let Some(e) = errors.first() {
                return Err(e.clone());
            }
            {
                let mut ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let w = ws.works.iter_mut().find(|w| w.path == paths[0]).ok_or("Это фото уже загружено в другую работу")?;
                if !student.is_empty() {
                    w.student = student.clone();
                }
                if paths.len() > 1 {
                    core.attach_sources(w, &paths[1..]);
                }
            }
            core.compute_plagiarism();
            core.save_session();
            Ok(core.state_json())
        }

        "journal_student" => {
            // убрать студента из группы (список, варианты, пары, оценки) или переименовать / объединить
            let group = p.get("group").str_or("");
            let student = p.get("student").str_or("");
            if student.is_empty() {
                return Err("Не указан студент".into());
            }
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                match p.get("action").str_or("").as_str() {
                    "delete" => {
                        j.delete_student(&group, &student);
                    }
                    "rename" => {
                        let to = p.get("to").str_or("").trim().to_string();
                        if to.is_empty() {
                            return Err("Укажите новое ФИО".into());
                        }
                        j.rename_student(&group, &student, &to);
                    }
                    _ => return Err("Неизвестное действие".into()),
                }
                j.save();
            }
            core.compute_plagiarism();
            Ok(journal_view(core, &p))
        }

        "journal_set" => {
            // ручная правка ячейки: grade = null — удалить
            let (group, student, code) = (p.get("group").str_or(""), p.get("student").str_or(""), p.get("code").str_or(""));
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                let custom = j.kind_of(&group, &code) == "custom";
                match p.get("grade").as_f64() {
                    Some(g) => j.upsert(Entry {
                        // как вписал преподаватель, до сотых (2,6 из Moodle не превращается в 2,5)
                        grade: Some(if custom { g } else { (g.clamp(0.0, 5.0) * 100.0).round() / 100.0 }),
                        group,
                        student,
                        code,
                        violation: String::new(),
                        date: super::core::now_s(),
                        file: "вручную".into(),
                        variant: String::new(),
                        flag: super::journal::comment_flags(&p.get("comment").str_or("")),
                        comment: p.get("comment").str_or(""),
                        style: Vec::new(),
                    }),
                    None => j.entries.retain(|e| !(e.group == group && e.student == student && e.code == code)),
                }
                j.save();
            }
            Ok(journal_view(core, &p))
        }

        "journal_topic" => {
            // тема журнала: задать вид/предмет/семестр/даты, переименовать, удалить целиком
            let group = p.get("group").str_or("");
            let code = p.get("code").str_or("").trim().to_string();
            if code.is_empty() {
                return Err("Не указана тема".into());
            }
            let groups: Vec<String> = { let mut g = p.get("groups").str_list(); if g.is_empty() { g.push(group.clone()); } g };
            let mut n = 0;
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                for g in &groups {
                    match p.get("action").str_or("set").as_str() {
                        "set" => {
                            j.ensure_group(g);
                            // заголовок и шкала Moodle в форме не редактируются — сохраняем прежние
                            let old = j.topic(g, &code);
                            let mut t = super::journal::Topic::from_json(p.get("topic"));
                            if t.moodle.is_empty() {
                                t.moodle = old.moodle;
                            }
                            if t.mscale.is_empty() {
                                t.mscale = old.mscale;
                            }
                            // короткое имя меняется только там, где его поле есть в форме
                            if p.get("topic").get("short").is_null() {
                                t.short = old.short;
                            }
                            j.set_topic(g, &code, t);
                            // тема без даты и без оценок всё равно должна остаться столбцом
                            if j.topic(g, &code).is_empty() && !j.entries.iter().any(|e| &e.group == g && e.code == code) {
                                j.set_topic(g, &code, super::journal::Topic { kind: super::journal::auto_kind(&code, "").into(), ..Default::default() });
                            }
                        }
                        "delete" => n += j.delete_topic(g, &code),
                        "forget" => j.set_topic(g, &code, Default::default()),
                        "rename" => j.rename_topic(g, &code, p.get("to").str_or("").trim()),
                        _ => return Err("Неизвестное действие".into()),
                    }
                }
                j.save();
            }
            Ok(journal_view(core, &p).with("removed", n))
        }

        "journal_fill" => {
            // «2» всем, у кого нет оценки по теме (или по всем работам и тестам)
            let group = p.get("group").str_or("");
            let grade = p.get("grade").f64_or(2.0);
            let n = {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                let codes = { let c = p.get("codes").str_list(); if c.is_empty() { j.codes_of(&group) } else { c } };
                let n = j.fill_missing(&group, &codes, grade, super::core::now_s());
                j.save();
                n
            };
            Ok(journal_view(core, &p).with("filled", n))
        }

        "journal_averages" => {
            // свои средние группы: [{name, codes}]
            let group = p.get("group").str_or("");
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                let list: Vec<(String, Vec<String>)> = p.get("averages").as_arr().iter().map(|a| (a.get("name").str_or("").trim().to_string(), a.get("codes").str_list())).filter(|(n, c)| !n.is_empty() && !c.is_empty()).collect();
                if list.is_empty() {
                    j.averages.remove(&group);
                } else {
                    j.averages.insert(group, list);
                }
                j.save();
            }
            Ok(journal_view(core, &p))
        }

        "journal_calendar" => {
            // все темы всех групп с датами — для календаря
            let j = core.journal.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let mut items = Json::arr();
            for (g, m) in &j.topics {
                for (c, t) in m {
                    items.push(t.to_json().with("group", g.as_str()).with("code", c.as_str()).with("kind", j.kind_of(g, c)).with("graded", j.entries.iter().filter(|e| &e.group == g && &e.code == c && e.grade.is_some()).count()));
                }
            }
            // оценённые темы без календаря — тоже, с датой первой оценки
            let tz = report::TZ_MIN.load(Ordering::Relaxed) * 60;
            let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
            for e in &j.entries {
                if j.topics.get(&e.group).map(|m| m.contains_key(&e.code)).unwrap_or(false) || !seen.insert((e.group.clone(), e.code.clone())) {
                    continue;
                }
                let first = j.entries.iter().filter(|x| x.group == e.group && x.code == e.code && x.date > 0).map(|x| x.date).min().unwrap_or(0);
                items.push(Json::obj().with("group", e.group.as_str()).with("code", e.code.as_str()).with("kind", j.kind_of(&e.group, &e.code)).with("issued", if first > 0 { super::core::ymd(first as i64 + tz) } else { String::new() }).with("deadline", "").with("title", "").with("subject", "").with("note", "").with("auto", true).with("graded", 1));
            }
            Ok(Json::obj().with("items", items))
        }

        "set_subject" => {
            // предмет набора заданий
            let set = p.get("set").str_or("");
            let subject = p.get("subject").str_or("").trim().to_string();
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let mut m = st.get("set_subjects").clone();
                if m.is_null() {
                    m = Json::obj();
                }
                m.set(&set, subject.as_str());
                st.set("set_subjects", m);
                let mut subs = st.get("subjects").str_list();
                if !subject.is_empty() && !subs.contains(&subject) {
                    subs.push(subject.clone());
                    st.set("subjects", subs);
                }
            }
            core.save_settings();
            Ok(core.state_json())
        }

        "rubric_set" => {
            // рубрика набора заданий: критерии с баллами и пороги оценок; проверенные работы пересчитываются
            let set = p.get("set").str_or("");
            if set.is_empty() {
                return Err("Не указан набор заданий".into());
            }
            let r = super::core::Rubric::from_json(p.get("rubric"));
            if r.p5 <= r.p4 || r.p4 <= r.p3 || r.p3 <= 0.0 || r.p5 > 100.0 {
                return Err("Пороги оценок должны идти по возрастанию: «3» < «4» < «5» ≤ 100 %".into());
            }
            {
                let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                let mut m = st.get("rubrics").clone();
                if m.is_null() {
                    m = Json::obj();
                }
                if r.items.is_empty() {
                    m.remove(&set);
                } else {
                    let items: Vec<Json> = r.items.iter().map(|c| Json::obj().with("name", c.name.as_str()).with("max", c.max).with("desc", c.desc.as_str())).collect();
                    m.set(&set, Json::obj().with("items", Json::Arr(items)).with("scale", Json::obj().with("5", r.p5).with("4", r.p4).with("3", r.p3)));
                }
                st.set("rubrics", m);
            }
            core.save_settings();
            // пересчитать оценки уже проверенных работ этого набора
            let ids: Vec<String> = {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                ws.works.iter().filter(|w| core.set_for_work(&ws, w).map(|s| s.id == set).unwrap_or(false) && ws.outcomes.get(&w.id).map(|o| !o.llm.is_null()).unwrap_or(false)).map(|w| w.id.clone()).collect()
            };
            for id in &ids {
                let _ = core.refinalize(id);
            }
            core.save_session();
            Ok(core.state_json().with("recalculated", ids.len()))
        }

        // ------------------------------------------------ архив работ прошлых лет
        "archive_summary" => Ok(super::history::arc_summary(&core.data_dir).with("settings", core.settings().get("archive").clone())),

        "archive_list" => {
            let code = p.get("code").str_or("");
            let list: Vec<Json> = super::history::arc_load(&core.data_dir).iter().filter(|e| code.is_empty() || e.code == code).map(|e| e.meta().with("words", e.shingles.len())).collect();
            Ok(Json::obj().with("items", Json::Arr(list)))
        }

        "archive_add_paths" => {
            // работы прошлых лет из папки или файлов: только отпечатки текста, в текущую проверку не попадают
            let code = p.get("code").str_or("").trim().to_string();
            if code.is_empty() {
                return Err("Укажите, какая это работа (шифр), например ПР3".into());
            }
            let group = p.get("group").str_or("").trim().to_string();
            let year = p.get("year").i64_or(0);
            // дата записи — 1 сентября указанного учебного года
            let date = if year > 1990 { ((year - 1970) as f64 * 365.2425 * 86400.0 + 243.0 * 86400.0) as u64 } else { super::core::now_s() };
            let files = super::core::Core::expand(&p.get("paths").str_list());
            let (task_sh, title): (std::collections::HashSet<u64>, String) = {
                let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                let set = ws.sets.iter().find(|s| s.code == code);
                (
                    set.map(|s| s.variants.values().flat_map(|v| super::detect::shingle_set(&format!("{}\n{}", v.task_visible, v.key))).collect()).unwrap_or_default(),
                    set.map(|s| s.title.clone()).unwrap_or_default(),
                )
            };
            let mut items = Vec::new();
            let mut skipped: Vec<String> = Vec::new();
            for (f, _) in files {
                let Ok(bytes) = std::fs::read(&f) else { continue };
                let name = super::core::file_name(&f);
                let text = match super::core::extract_safe(&f, &bytes) {
                    Ok(x) if !x.needs_pdf && !x.needs_images => x.visible(),
                    _ => {
                        skipped.push(name);
                        continue;
                    }
                };
                if text.split_whitespace().count() < 40 {
                    skipped.push(name);
                    continue;
                }
                // ФИО — из имени файла Moodle или из имени файла/папки
                let student = super::moodle::parse_name(&name).map(|x| x.0).unwrap_or_else(|| {
                    let stem = std::path::Path::new(&name).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    stem.replace(['_', '-'], " ").split_whitespace().take(3).collect::<Vec<_>>().join(" ")
                });
                items.push(super::history::ArcEntry {
                    code: code.clone(),
                    title: title.clone(),
                    group: group.clone(),
                    student,
                    date,
                    hash: format!("{:x}", super::core::fnv(&bytes)),
                    shingles: super::detect::shingle_set(&text).difference(&task_sh).copied().collect(),
                    ..Default::default()
                });
            }
            let n = super::history::arc_add(&core.data_dir, items);
            core.compute_plagiarism();
            Ok(super::history::arc_summary(&core.data_dir).with("added", n).with("skipped", skipped))
        }

        "archive_delete" => {
            let n = super::history::arc_delete(&core.data_dir, &p.get("ids").str_list(), &p.get("code").str_or(""));
            core.compute_plagiarism();
            Ok(super::history::arc_summary(&core.data_dir).with("removed", n))
        }

        "data_dir_move" => {
            // перенести настройки, сессию и журнал в другую папку; вступает в силу после перезапуска
            let to = std::path::PathBuf::from(p.get("dir").str_or("").trim());
            let std_dir = super::core::standard_data_dir();
            if to.as_os_str().is_empty() {
                return Err("Выберите папку".into());
            }
            if super::core::portable_dir().map(|d| d == core.data_dir).unwrap_or(false) {
                return Err("Включён переносной режим: данные лежат в папке «Данные» рядом с программой. Чтобы перенести их, сначала выключите переносной режим (удалите portable.txt рядом с программой).".into());
            }
            // «вернуть стандартную» — тоже перенос, в стандартную папку
            let to = if p.get("reset").bool_or(false) { std_dir.clone() } else { to };
            if to == core.data_dir {
                return Err("Данные уже лежат в этой папке".into());
            }
            if to.starts_with(&core.data_dir) {
                return Err("Нельзя перенести данные внутрь их же папки".into());
            }
            std::fs::create_dir_all(&to).map_err(|e| format!("Не удалось создать папку {}: {}", to.display(), e))?;
            if to != std_dir && to.join("settings.json").exists() && !p.get("use_existing").bool_or(false) {
                return Ok(Json::obj().with("exists", true).with("dir", to.to_string_lossy().to_string()));
            }
            core.save_session();
            core.save_settings();
            core.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
            let mut n = 0;
            if !(to.join("settings.json").exists() && p.get("use_existing").bool_or(false)) {
                fn copy_dir(a: &std::path::Path, b: &std::path::Path, skip: &std::path::Path) -> usize {
                    let _ = std::fs::create_dir_all(b);
                    let mut n = 0;
                    if let Ok(rd) = std::fs::read_dir(a) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p == skip || e.file_name() == "moved_to.txt" {
                                continue;
                            }
                            let t = b.join(e.file_name());
                            if p.is_dir() {
                                n += copy_dir(&p, &t, skip);
                            } else if std::fs::copy(&p, &t).is_ok() {
                                n += 1;
                            }
                        }
                    }
                    n
                }
                n = copy_dir(&core.data_dir, &to, &to);
                // прежняя папка — чтобы пути в сессии поправились при первом запуске из новой
                let _ = std::fs::write(to.join("data_dir.txt"), core.data_dir.to_string_lossy().as_bytes());
            }
            let _ = std::fs::create_dir_all(&std_dir);
            if to == std_dir {
                let _ = std::fs::remove_file(std_dir.join("moved_to.txt"));
            } else {
                std::fs::write(std_dir.join("moved_to.txt"), to.to_string_lossy().as_bytes()).map_err(|e| format!("Не удалось запомнить новую папку: {}", e))?;
            }
            core.log(&format!("Папка данных: {} → {} (после перезапуска)", core.data_dir.display(), to.display()));
            Ok(Json::obj().with("dir", to.to_string_lossy().to_string()).with("files", n).with("restart", true))
        }

        "portable_info" => {
            let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
            Ok(Json::obj()
                .with("portable", super::core::portable_dir().map(|d| d == core.data_dir).unwrap_or(false))
                .with("data_dir", core.data_dir.to_string_lossy().to_string())
                .with("standard", core.data_dir == super::core::standard_data_dir())
                .with("exe_dir", exe_dir.map(|d| d.to_string_lossy().to_string()).unwrap_or_default()))
        }

        "portable_enable" => {
            // переносной режим: данные — в папку «Данные» рядом с программой; вступит в силу после перезапуска
            let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).ok_or("Не удалось определить папку программы")?;
            let dest = exe_dir.join("Данные");
            if dest == core.data_dir {
                return Err("Переносной режим уже включён".into());
            }
            if dest.join("settings.json").exists() {
                return Err(format!("В папке «{}» уже есть данные — переносной режим включится сам при следующем запуске", dest.display()));
            }
            std::fs::create_dir_all(&dest).map_err(|e| format!("Не удалось создать папку рядом с программой ({}): {}. Переносной режим работает, если программа лежит в папке, куда можно писать (например, на флешке).", dest.display(), e))?;
            core.save_session();
            core.save_settings();
            core.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
            fn copy_dir(a: &std::path::Path, b: &std::path::Path) -> usize {
                let _ = std::fs::create_dir_all(b);
                let mut n = 0;
                if let Ok(rd) = std::fs::read_dir(a) {
                    for e in rd.flatten() {
                        let p = e.path();
                        let t = b.join(e.file_name());
                        if p.is_dir() {
                            n += copy_dir(&p, &t);
                        } else if std::fs::copy(&p, &t).is_ok() {
                            n += 1;
                        }
                    }
                }
                n
            }
            let n = copy_dir(&core.data_dir, &dest);
            // прежняя папка — чтобы пути в сессии поправились при первом запуске из «Данных»
            let _ = std::fs::write(dest.join("data_dir.txt"), core.data_dir.to_string_lossy().as_bytes());
            let _ = std::fs::write(exe_dir.join("portable.txt"), "Переносной режим «Проверки заданий»: данные лежат в папке «Данные» рядом с программой.\r\nЧтобы вернуться к обычному режиму, удалите этот файл и папку «Данные» (сначала сделайте экспорт).\r\n");
            Ok(Json::obj().with("dir", dest.to_string_lossy().to_string()).with("files", n))
        }

        "calibration" => Ok(super::history::cal_summary(&core.data_dir, &p.get("code").str_or(""))),

        // ------------------------------------------------ отчёты по журналу
        "journal_debtors" => {
            // кто что должен: по группам, для куратора
            let path = p.get("path").str_or("");
            let only = p.get("group").str_or("");
            let twos = p.get("twos").bool_or(true);
            let v = journal_view(core, p);
            let mut rows_all: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new(); // группа, ФИО, долги, «2»
            for g in v.get("groups").as_arr() {
                let gname = g.get("name").str_or("");
                if !only.is_empty() && gname != only {
                    continue;
                }
                let topics: Vec<&Json> = g.get("topics").as_arr().iter().filter(|t| t.get("kind").str_or("") != "custom").collect();
                for s in g.get("students").as_arr() {
                    if !s.get("in_roster").bool_or(true) {
                        continue;
                    }
                    let mut debts = Vec::new();
                    let mut low = Vec::new();
                    for t in &topics {
                        let code = t.get("code").str_or("");
                        let label = { let sh = t.get("short").str_or(""); if sh.is_empty() { code.clone() } else { sh } };
                        let c = s.get("cells").get(&code);
                        match c.get("grade").as_f64() {
                            None if t.get("due").bool_or(false) => debts.push(label),
                            Some(x) if twos && x < 2.5 => low.push(format!("{} («{}»)", label, super::core::fmt_grade(x))),
                            _ => {}
                        }
                    }
                    if !debts.is_empty() || !low.is_empty() {
                        rows_all.push((gname.clone(), s.get("name").str_or(""), debts, low));
                    }
                }
            }
            let title = if only.is_empty() { "Задолженности студентов".to_string() } else { format!("Задолженности студентов — {}", only) };
            let heads: Vec<String> = vec!["№".into(), "Группа".into(), "Студент".into(), "Не сдано".into(), if twos { "На «2» (пересдать)".into() } else { String::new() }, "Всего".into()].into_iter().filter(|x: &String| !x.is_empty()).collect();
            let rows: Vec<Vec<String>> = rows_all
                .iter()
                .enumerate()
                .map(|(i, (g, n, d, l))| {
                    let mut r = vec![(i + 1).to_string(), g.clone(), n.clone(), d.join(", ")];
                    if twos {
                        r.push(l.join(", "));
                    }
                    r.push((d.len() + l.len()).to_string());
                    r
                })
                .collect();
            let filt = [p.get("f_semester").str_or(""), p.get("f_subject").str_or("")].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(", ");
            let sub = format!("Сформировано {}{}. Долг — работа, которую уже сдавали другие или у которой прошёл срок.", super::report::today_str(), if filt.is_empty() { String::new() } else { format!(" ({})", filt) });
            let bytes = if path.to_lowercase().ends_with(".xlsx") {
                let widths: Vec<u32> = heads.iter().map(|h| match h.as_str() { "№" => 5, "Группа" => 12, "Студент" => 30, "Всего" => 8, _ => 40 }).collect();
                let fills: Vec<Vec<String>> = rows.iter().map(|r| r.iter().map(|_| String::new()).collect()).collect();
                super::report::xlsx_table_ex("Долги", &format!("{}  ({})", title, sub), &heads, &rows, &widths, &fills, 3)
            } else {
                let widths: Vec<u32> = heads.iter().map(|h| match h.as_str() { "№" => 500, "Группа" => 1200, "Студент" => 3000, "Всего" => 800, _ => 4200 }).collect();
                let trows: Vec<Vec<(String, String)>> = rows.iter().map(|r| r.iter().map(|x| (x.clone(), String::new())).collect()).collect();
                let mut blocks = vec![Block::Title(title.clone()), Block::Sub(sub)];
                if rows.is_empty() {
                    blocks.push(Block::Para("Задолженностей нет.".into()));
                } else {
                    blocks.push(Block::Table { heads: heads.clone(), widths, rows: trows });
                }
                super::report::docx_blocks(&title, &blocks, true)
            };
            std::fs::write(&path, bytes).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path).with("students", rows_all.len()))
        }

        "journal_student_report" => {
            // всё о студенте на одной странице: оценки по темам, средние, долги, пропуски, нарушения
            let path = p.get("path").str_or("");
            let group = p.get("group").str_or("");
            let student = p.get("student").str_or("");
            let v = journal_view(core, p);
            let g = v.get("groups").as_arr().iter().find(|g| g.get("name").str_or("") == group).cloned().ok_or("Группа не найдена")?;
            let s = g.get("students").as_arr().iter().find(|s| s.get("name").str_or("") == student).cloned().ok_or("Студент не найден")?;
            let num = |x: &Json| x.as_f64().map(super::core::fmt_grade).unwrap_or_else(|| "—".into());
            let mut blocks = vec![Block::Title(student.clone()), Block::Sub(format!("Группа {}. Сформировано {}.", group, super::report::today_str()))];
            let mut avgs = vec![format!("Средний балл: {}", num(s.get("avg")))];
            if g.get("has_works").bool_or(false) && g.get("has_tests").bool_or(false) {
                avgs.push(format!("по работам: {}", num(s.get("avg_works"))));
                avgs.push(format!("по тестам: {}", num(s.get("avg_tests"))));
            }
            for (name, val) in s.get("avg_own").as_obj() {
                avgs.push(format!("{}: {}", name, num(val)));
            }
            blocks.push(Block::Bold(avgs.join(" • ")));
            let mut rows: Vec<Vec<(String, String)>> = Vec::new();
            let mut debts: Vec<String> = Vec::new();
            for t in g.get("topics").as_arr() {
                let code = t.get("code").str_or("");
                let kind = t.get("kind").str_or("");
                let c = s.get("cells").get(&code);
                let grade = c.get("grade").as_f64();
                if grade.is_none() && t.get("due").bool_or(false) {
                    debts.push(code.clone());
                }
                let gtxt = match grade {
                    Some(x) if kind == "custom" => format!("{}{}", super::core::fmt_grade(x), { let u = t.get("unit").str_or(""); if u.is_empty() { String::new() } else { format!(" {}", u) } }),
                    Some(x) => super::core::fmt_grade(x),
                    None if t.get("due").bool_or(false) => "не сдано".into(),
                    None => "—".into(),
                };
                let viol = match c.get("violation").str_or("").as_str() { "ai" => "ИИ", "plagiarism" => "списывание", _ => "" };
                let fill = if kind == "custom" { "" } else { match grade.map(super::report::grade_bucket) { Some(x) if x >= 5.0 => "D9F2E3", Some(x) if x >= 4.0 => "DCEBFB", Some(x) if x >= 3.0 => "FFF1CC", Some(_) => "FBDADA", None => "" } };
                let title = t.get("title").str_or("");
                let kind_l = match kind.as_str() { "test" => "тест", "custom" => "", _ => "работа" };
                rows.push(vec![
                    (if title.is_empty() { code.clone() } else { format!("{} — {}", code, title) }, String::new()),
                    (kind_l.into(), String::new()),
                    (gtxt, fill.into()),
                    (viol.into(), if viol.is_empty() { String::new() } else { "FBDADA".into() }),
                    (c.get("comment").str_or("").chars().take(300).collect(), String::new()),
                ]);
            }
            blocks.push(Block::Table { heads: vec!["Тема".into(), "Вид".into(), "Оценка".into(), "Нарушение".into(), "Комментарий".into()], widths: vec![4200, 900, 1100, 1300, 6000], rows });
            blocks.push(Block::Para(if debts.is_empty() { "Задолженностей нет.".into() } else { format!("Не сдано: {}.", debts.join(", ")) }));
            std::fs::write(&path, super::report::docx_blocks(&student, &blocks, true)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path))
        }

        "defense_plan" => {
            // кого вызвать на устную защиту: признаки ИИ, списывание, совпадение с архивом — с вопросами
            let path = p.get("path").str_or("");
            let only = p.get("group").str_or("");
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let mut items: Vec<(String, String, String, Vec<String>, Json)> = Vec::new(); // группа, студент, работа, причины, вопросы
            for w in &ws.works {
                if !only.is_empty() && w.group != only {
                    continue;
                }
                let Some(o) = ws.outcomes.get(&w.id) else { continue };
                let mut why: Vec<String> = Vec::new();
                match o.violation.as_str() {
                    "ai" => why.push(format!("признаки ИИ{}", if o.ai_evidence.is_empty() { String::new() } else { format!(": {}", o.ai_evidence.iter().take(2).cloned().collect::<Vec<_>>().join("; ")) })),
                    "plagiarism" => why.push(format!("совпадает с работой: {}", o.plag.iter().map(|(n, sc)| format!("{} ({} %)", n, (sc * 100.0).round())).collect::<Vec<_>>().join(", "))),
                    _ => {}
                }
                if o.violation.is_empty() && !o.ai_suspect.is_null() && o.ai_decision.is_empty() {
                    why.push("есть подозрение на ИИ — решение не принято".into());
                }
                if let Some(h) = ws.archive_hits.get(&w.id) {
                    why.push(format!("совпадает с работой прошлых лет: {}", h.iter().map(|(l, sc)| format!("{} ({} %)", l, (sc * 100.0).round())).collect::<Vec<_>>().join(", ")));
                }
                if why.is_empty() {
                    continue;
                }
                let (who, _) = core.authors(&ws, w);
                let code = core.set_for_work(&ws, w).map(|s| s.code.clone()).unwrap_or_default();
                items.push((w.group.clone(), who.join(", "), if code.is_empty() { w.name.clone() } else { format!("{} ({})", code, w.name) }, why, o.oral.get("questions").clone()));
            }
            drop(ws);
            items.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
            let title = if only.is_empty() { "План устной защиты".to_string() } else { format!("План устной защиты — {}", only) };
            let mut blocks = vec![Block::Title(title.clone()), Block::Sub(format!("Сформировано {}. Студентов: {}. Признаки — подсказка нейросети и автоматических проверок, решение за преподавателем.", super::report::today_str(), items.len()))];
            let mut no_q = 0;
            for (i, (g, who, work, why, qs)) in items.iter().enumerate() {
                blocks.push(Block::Heading(format!("{}. {} — {}", i + 1, who, g)));
                blocks.push(Block::Para(format!("Работа: {}", work)));
                blocks.push(Block::Para(format!("Почему: {}.", why.join("; "))));
                if qs.as_arr().is_empty() {
                    no_q += 1;
                    blocks.push(Block::Para("Вопросы не составлены (карточка работы → «Вопросы для устной защиты»).".into()));
                } else {
                    for (k, q) in qs.as_arr().iter().enumerate() {
                        let exp = q.get("expect").str_or("");
                        blocks.push(Block::Para(format!("{}) {}{}", k + 1, q.get("q").str_or(""), if exp.is_empty() { String::new() } else { format!(" — ожидается: {}", exp) })));
                    }
                }
                blocks.push(Block::Para("Итог защиты: ☐ защитил(а), оценка ____   ☐ не защитил(а)".into()));
            }
            if items.is_empty() {
                blocks.push(Block::Para("Работ с признаками ИИ или списывания нет — защита не нужна.".into()));
            }
            std::fs::write(&path, super::report::docx_blocks(&title, &blocks, false)).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path).with("students", items.len()).with("no_questions", no_q))
        }

        "defense_candidates" => {
            // работы для защиты без вопросов — интерфейс составит вопросы по очереди
            let only = p.get("group").str_or("");
            let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<String> = ws
                .works
                .iter()
                .filter(|w| only.is_empty() || w.group == only)
                .filter(|w| {
                    ws.outcomes.get(&w.id).map(|o| (!o.violation.is_empty() || (!o.ai_suspect.is_null() && o.ai_decision.is_empty()) || ws.archive_hits.contains_key(&w.id)) && o.oral.get("questions").as_arr().is_empty() && !o.llm.is_null()).unwrap_or(false)
                })
                .map(|w| w.id.clone())
                .collect();
            Ok(Json::obj().with("ids", ids))
        }

        "reminders_check" => {
            // сроки сдачи из календаря: скоро срок, а работу сдали не все
            let rem = core.settings().get("reminders").clone();
            let days = rem.get("days").f64_or(2.0).max(0.0);
            let v = journal_view(core, &Json::obj());
            let now = super::core::now_s() as i64;
            let tz = report::TZ_MIN.load(Ordering::Relaxed) * 60;
            let mut list: Vec<Json> = Vec::new();
            let mut sent = rem.get("sent").clone();
            if sent.is_null() {
                sent = Json::obj();
            }
            let mut to_send: Vec<(String, String)> = Vec::new();
            for g in v.get("groups").as_arr() {
                let gname = g.get("name").str_or("");
                let roster: Vec<&Json> = g.get("students").as_arr().iter().filter(|s| s.get("in_roster").bool_or(true)).collect();
                for t in g.get("topics").as_arr() {
                    let dl = t.get("deadline").str_or("");
                    if dl.is_empty() || t.get("kind").str_or("") == "custom" {
                        continue;
                    }
                    let Some(at) = super::journal::parse_local(&dl) else { continue };
                    let at = at - tz;
                    let left = at - now;
                    if left < 0 || left as f64 > days * 86400.0 {
                        continue;
                    }
                    let code = t.get("code").str_or("");
                    let missing: Vec<String> = roster.iter().filter(|s| s.get("cells").get(&code).get("grade").is_null()).map(|s| s.get("name").str_or("")).collect();
                    if missing.is_empty() {
                        continue;
                    }
                    let item = Json::obj().with("group", gname.as_str()).with("code", code.as_str()).with("title", t.get("title").str_or("").as_str()).with("deadline", dl.as_str()).with("hours_left", (left / 3600) as i64).with("missing", missing.len()).with("of", roster.len());
                    list.push(item);
                    let key = format!("{}|{}|{}", gname, code, dl);
                    if sent.get(&key).is_null() {
                        sent.set(&key, now as f64);
                        let when = if left < 86400 { format!("через {} ч", (left / 3600).max(1)) } else { format!("через {} дн.", (left + 43199) / 86400) };
                        to_send.push((format!("Скоро срок {} у {}", code, gname), format!("Срок {} ({}), не сдали {} из {}", when, dl.replace('T', " "), missing.len(), roster.len())));
                    }
                }
            }
            if !to_send.is_empty() && rem.get("enabled").bool_or(true) {
                {
                    let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
                    let mut r = st.get("reminders").clone();
                    if r.is_null() {
                        r = Json::obj();
                    }
                    r.set("sent", sent);
                    st.set("reminders", r);
                }
                core.save_settings();
                let st = core.settings();
                if super::notify::wanted(&st, "deadline") {
                    for (t, b) in &to_send {
                        super::notify::send(core, "deadline", t, b);
                    }
                }
            }
            Ok(Json::obj().with("items", Json::Arr(list)).with("notified", to_send.len()))
        }

        "journal_clear" => {
            {
                let mut j = core.journal.lock().unwrap_or_else(|e| e.into_inner());
                match p.get("group").as_str() {
                    Some(g) => j.entries.retain(|e| e.group != g),
                    None => j.entries.clear(),
                }
                j.save();
            }
            Ok(journal_view(core, &p))
        }

        "journal_export" => {
            // журнал в Excel или Word: какие столбцы — по галочкам (`cols`), темы — по фильтру семестра и предмета
            let path = p.get("path").str_or("");
            let fmt = p.get("format").str_or("xlsx");
            let only = p.get("group").as_str().map(|s| s.to_string());
            let cols = p.get("cols").clone();
            let on = |k: &str, d: bool| cols.get(k).bool_or(d);
            let v = journal_view(core, &p);
            let groups: Vec<Json> = v.get("groups").as_arr().iter().filter(|g| only.as_ref().map(|o| o == &g.get("name").str_or("")).unwrap_or(true)).cloned().collect();
            // темы: из выбранной группы или объединение всех
            let mut codes: Vec<String> = Vec::new();
            let mut kinds: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for g in &groups {
                for t in g.get("topics").as_arr() {
                    let c = t.get("code").str_or("");
                    let k = t.get("kind").str_or("work");
                    if k == "custom" && !on("custom", true) {
                        continue;
                    }
                    if !codes.contains(&c) {
                        codes.push(c.clone());
                    }
                    kinds.insert(c, k);
                }
            }
            // по порядку: работы, тесты, свои столбцы
            let rank = |c: &String| match kinds.get(c).map(|k| k.as_str()) {
                Some("test") => 1u8,
                Some("custom") => 2,
                _ => 0,
            };
            codes.sort_by_key(|c| (rank(c), natural_key(c)));
            let own_names: Vec<String> = {
                let mut n: Vec<String> = Vec::new();
                for g in &groups {
                    for a in g.get("averages").as_arr() {
                        let x = a.get("name").str_or("");
                        if !n.contains(&x) {
                            n.push(x);
                        }
                    }
                }
                n
            };
            let has_tests = codes.iter().any(|c| kinds.get(c).map(|k| k == "test").unwrap_or(false));
            let has_works = codes.iter().any(|c| kinds.get(c).map(|k| k == "work").unwrap_or(false));
            let with_group = only.is_none();
            let mut heads: Vec<String> = Vec::new();
            if on("num", true) {
                heads.push("№".into());
            }
            heads.push("Студент".into());
            if with_group {
                heads.push("Группа".into());
            }
            let first_grade = heads.len();
            heads.extend(codes.iter().cloned());
            let last_grade = heads.len();
            let fmt2 = |x: &Json| x.as_f64().map(|a| format!("{:.2}", a).replace('.', ",")).unwrap_or_default();
            let mut extra: Vec<(&str, String)> = Vec::new();
            if on("avg", true) {
                extra.push(("avg", "Средний".into()));
            }
            if on("avg_works", true) && has_works && has_tests {
                extra.push(("avg_works", "Средний по работам".into()));
            }
            if on("avg_tests", true) && has_tests {
                extra.push(("avg_tests", "Средний по тестам".into()));
            }
            if on("avg_own", true) {
                for n in &own_names {
                    extra.push(("own", n.clone()));
                }
            }
            if on("violations", true) {
                extra.push(("violations", "Нарушения".into()));
            }
            if on("missing", true) {
                extra.push(("missing", "Долги".into()));
            }
            heads.extend(extra.iter().map(|(_, h)| h.clone()));
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut n = 0;
            for g in &groups {
                let gname = g.get("name").str_or("");
                for st in g.get("students").as_arr() {
                    n += 1;
                    let mut r: Vec<String> = Vec::new();
                    if on("num", true) {
                        r.push(n.to_string());
                    }
                    r.push(st.get("name").str_or(""));
                    if with_group {
                        r.push(gname.clone());
                    }
                    for c in &codes {
                        let cell = st.get("cells").get(c);
                        let custom = kinds.get(c).map(|k| k == "custom").unwrap_or(false);
                        r.push(match cell.get("grade").as_f64() {
                            Some(x) if custom => if (x - x.round()).abs() < 1e-9 { format!("{}", x.round() as i64) } else { format!("{}", x).replace('.', ",") },
                            Some(x) => format!("{}{}{}", super::core::fmt_grade(x), if cell.get("violation").str_or("").is_empty() { "" } else { "*" }, if cell.get("flag").str_or("").is_empty() { "" } else { "•" }),
                            None => String::new(),
                        });
                    }
                    for (k, name) in &extra {
                        r.push(match *k {
                            "avg" | "avg_works" | "avg_tests" => fmt2(st.get(k)),
                            "own" => fmt2(st.get("avg_own").get(name)),
                            other => st.get(other).i64_or(0).to_string(),
                        });
                    }
                    rows.push(r);
                }
            }
            let filt = v.get("filter");
            let mut sub_title: Vec<String> = Vec::new();
            for k in ["subject", "semester"] {
                let x = filt.get(k).str_or("");
                if !x.is_empty() {
                    sub_title.push(x);
                }
            }
            let title = format!(
                "Журнал группы{}{}",
                only.as_ref().map(|g| format!(" {}", g)).unwrap_or_default(),
                if sub_title.is_empty() { String::new() } else { format!(" — {}", sub_title.join(", ")) }
            );
            let legend = "* — «2» за нарушение (ИИ или списывание); • — в отзыве упоминается ИИ; пустая клетка — оценки нет";
            // цвет — только у оценок; дробные («3,5») — по ближайшей целой
            let fill_of = |i: usize, x: &str| -> &'static str {
                if i < first_grade || i >= last_grade || kinds.get(&heads[i]).map(|k| k == "custom").unwrap_or(false) {
                    return "";
                }
                match x.trim_end_matches(['*', '•']).replace(',', ".").parse::<f64>().ok().map(|g| super::report::grade_bucket(g) as i64) {
                    Some(5) => "C6EFCE",
                    Some(4) => "DDEBF7",
                    Some(3) => "FFEB9C",
                    Some(0..=2) => "F8CBAD",
                    _ => "",
                }
            };
            let bytes = if fmt == "docx" {
                let mut widths: Vec<u32> = Vec::new();
                if on("num", true) {
                    widths.push(500);
                }
                widths.push(3400);
                if with_group {
                    widths.push(1100);
                }
                widths.extend(codes.iter().map(|_| 900u32));
                widths.extend(extra.iter().map(|_| 1100u32));
                let trows: Vec<Vec<(String, String)>> = rows.iter().map(|r| r.iter().enumerate().map(|(i, x)| (x.clone(), fill_of(i, x).to_string())).collect()).collect();
                super::report::docx_blocks(&title, &[Block::Title(title.clone()), Block::Sub(format!("Сформировано {}. {}", super::report::today_str(), legend)), Block::Table { heads: heads.clone(), widths, rows: trows }], true)
            } else {
                let mut widths: Vec<u32> = Vec::new();
                if on("num", true) {
                    widths.push(5);
                }
                widths.push(34);
                if with_group {
                    widths.push(10);
                }
                widths.extend(codes.iter().map(|c| (c.chars().count() as u32 + 2).clamp(8, 22)));
                widths.extend(extra.iter().map(|(_, h)| (h.chars().count() as u32 + 2).clamp(9, 22)));
                let fills: Vec<Vec<String>> = rows.iter().map(|r| r.iter().enumerate().map(|(i, x)| fill_of(i, x).to_string()).collect()).collect();
                super::report::xlsx_table_ex("Журнал", &format!("{}  ({})", title, legend), &heads, &rows, &widths, &fills, if on("num", true) { 2 } else { 1 })
            };
            std::fs::write(&path, bytes).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path))
        }

        // ссылка из работы студента — в браузере по умолчанию (без cmd: в адресе бывают & и ^)
        "open_url" => {
            let url = p.get("url").str_or("");
            let l = url.to_lowercase();
            if !(l.starts_with("http://") || l.starts_with("https://")) || url.chars().any(|c| c.is_control() || c == '"' || c == ' ') {
                return Err("Это не ссылка на сайт".into());
            }
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", &url]).spawn();
            }
            #[cfg(not(target_os = "windows"))]
            open_in_os(&url, false);
            Ok(Json::obj().with("ok", true))
        }

        // как будет выглядеть текст после обезличивания
        "anon_preview" => {
            let names: Vec<String> = p.get("names").str_list();
            let a = if names.is_empty() {
                // берём те же ФИО, что и при проверке (без учёта настройки)
                let mut all: Vec<String> = Vec::new();
                {
                    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
                    for w in &ws.works {
                        all.extend(core.authors(&ws, w).0);
                    }
                }
                all.extend(core.journal.lock().unwrap_or_else(|e| e.into_inner()).rosters.values().flatten().cloned());
                all.retain(|n| !n.trim().is_empty() && !n.starts_with('(') && n != "не определён");
                super::anon::Anon::new(&all)
            } else {
                super::anon::Anon::new(&names)
            };
            Ok(Json::obj().with("text", a.apply(&p.get("text").str_or(""))).with("people", a.len()))
        }

        "notify_test" => {
            super::notify::send(core, "test", "Проверка заданий", "Так будут выглядеть уведомления: например, «Проверка завершена — проверено работ: 24».");
            Ok(Json::obj().with("ok", true))
        }

        // ---- обновления с GitHub
        "update_check" => {
            if p.get("auto").bool_or(false) && !super::updater::due(&core.settings()) {
                return Ok(Json::obj().with("skipped", true));
            }
            let r = super::updater::check(core)?;
            let skip = core.settings().get("updates").get("skip").str_or("");
            if p.get("auto").bool_or(false) && r.get("available").bool_or(false) && r.get("version").str_or("") != skip {
                super::notify::send(core, "update", "Вышла новая версия", &format!("Проверка заданий {} — откройте приложение, чтобы обновиться.", r.get("version").str_or("")));
            }
            Ok(r)
        }
        "update_download" => {
            let script = super::updater::download(core, &p.get("url").str_or(""), &p.get("version").str_or(""))?;
            Ok(Json::obj().with("script", script))
        }
        "update_apply" => {
            super::updater::apply_and_exit(core, &p.get("script").str_or(""))?;
            Ok(Json::obj().with("ok", true))
        }

        // ---- перенос всего приложения одним файлом
        "export_all" => super::transfer::export(core, &p.get("path").str_or(""), super::transfer::Parts::from_json(p.get("parts"))),
        "import_inspect" => super::transfer::inspect(&p.get("path").str_or("")),
        "import_all" => {
            let r = super::transfer::import(core, &p.get("path").str_or(""), super::transfer::Parts::from_json(p.get("parts")))?;
            Ok(core.state_json().with("imported", r.get("imported").clone()))
        }

        // ---- свои темы и шрифты
        "read_text" => {
            let path = p.get("path").str_or("");
            let ext = super::core::ext_of(&path);
            if !["json", "txt", "proverka-theme"].contains(&ext.as_str()) {
                return Err("Нужен файл .json".into());
            }
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if meta.len() > 30_000_000 {
                return Err("Файл слишком большой".into());
            }
            Ok(Json::obj().with("text", std::fs::read_to_string(&path).map_err(|e| format!("не удалось прочитать: {}", e))?))
        }
        "font_import" => {
            // .ttf/.otf/.woff/.woff2 — копия в папку данных/fonts
            let path = p.get("path").str_or("");
            let ext = super::core::ext_of(&path);
            if !["ttf", "otf", "woff", "woff2"].contains(&ext.as_str()) {
                return Err("Нужен файл шрифта: .ttf, .otf, .woff или .woff2".into());
            }
            let b = std::fs::read(&path).map_err(|e| e.to_string())?;
            let file = save_font(core, &super::core::file_name(&path), &b)?;
            Ok(Json::obj().with("file", file))
        }
        "font_save" => {
            let b = b64_decode(&p.get("b64").str_or(""));
            if b.is_empty() {
                return Err("пустой шрифт".into());
            }
            let file = save_font(core, &p.get("file").str_or("font.ttf"), &b)?;
            Ok(Json::obj().with("file", file))
        }
        "font_b64" => {
            let file = p.get("file").str_or("");
            if file.contains(['/', '\\']) || file.contains("..") {
                return Err("неверное имя".into());
            }
            let b = std::fs::read(core.data_dir.join("fonts").join(&file)).map_err(|e| e.to_string())?;
            Ok(Json::obj().with("b64", b64(&b)))
        }
        "font_delete" => {
            let file = p.get("file").str_or("");
            if file.contains(['/', '\\']) || file.contains("..") {
                return Err("неверное имя".into());
            }
            let _ = std::fs::remove_file(core.data_dir.join("fonts").join(&file));
            Ok(Json::obj().with("ok", true))
        }
        "copy_file" => {
            // экспорт шрифта: из папки данных — куда выбрал преподаватель
            let file = p.get("font").str_or("");
            if file.contains(['/', '\\']) || file.contains("..") {
                return Err("неверное имя".into());
            }
            std::fs::copy(core.data_dir.join("fonts").join(&file), p.get("to").str_or("")).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("ok", true))
        }

        // сохранить текст в файл, выбранный в окне «Сохранить как»
        "save_text" => {
            let path = p.get("path").str_or("");
            if path.is_empty() {
                return Err("Не выбран файл".into());
            }
            let bom = if p.get("bom").bool_or(false) { "\u{feff}" } else { "" };
            std::fs::write(&path, format!("{}{}", bom, p.get("text").str_or(""))).map_err(|e| format!("не удалось сохранить: {}", e))?;
            Ok(Json::obj().with("path", path))
        }

        "open_path" => {
            let path = p.get("path").str_or("");
            open_in_os(&path, p.get("select").bool_or(false));
            Ok(Json::obj().with("ok", true))
        }

        _ => Err(format!("неизвестная команда: {}", method)),
    }
}

pub fn open_in_os(path: &str, select: bool) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        if select {
            let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path)).spawn();
        } else {
            let _ = std::process::Command::new("cmd").args(["/C", "start", "", path]).creation_flags(CREATE_NO_WINDOW).spawn();
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = select;
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = select;
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// base64 → байты (допускается префикс «data:…;base64,»).
pub fn b64_decode(s: &str) -> Vec<u8> {
    let s = match s.find(";base64,") {
        Some(i) => &s[i + 8..],
        None => s,
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for ch in data.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[allow(dead_code)]
fn _unused(_: HashMap<String, String>) {}

/// Сводная таблица журнала: группы → студенты → оценки по работам.
pub fn journal_view(core: &Arc<Core>, p: &Json) -> Json {
    use super::journal::{parse_local, semester_of_date};
    let j = core.journal.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let st = core.settings();
    let opts = st.get("journal_opts").clone();
    let tests_min2 = opts.get("tests_min2").bool_or(false);
    // фильтр журнала (f_*: «subject» в запросах занят предметом импорта и темы)
    let f_sem = p.get("f_semester").str_or("");
    let f_sub = p.get("f_subject").str_or("");
    let tz = report::TZ_MIN.load(Ordering::Relaxed) * 60;
    let now = super::core::now_s() as i64;
    let mut groups: Vec<String> = j.groups.iter().map(|g| g.name.clone()).collect();
    for g in j.rosters.keys().chain(j.topics.keys()) {
        if !groups.contains(g) {
            groups.push(g.clone());
        }
    }
    for e in &j.entries {
        if !groups.contains(&e.group) {
            groups.push(e.group.clone());
        }
    }
    for w in core.ws.lock().unwrap_or_else(|e| e.into_inner()).works.iter() {
        if !w.group.is_empty() && !groups.contains(&w.group) {
            groups.push(w.group.clone());
        }
    }
    groups.sort_by_key(|g| natural_key(g));
    let mut semesters: Vec<String> = Vec::new();
    let mut subjects: Vec<String> = st.get("subjects").str_list();
    let mut all_codes: Vec<String> = Vec::new();
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let mut out = Json::arr();
    for g in &groups {
        let mut codes_all = j.codes_of(g);
        codes_all.sort_by_key(|c| natural_key(c));
        // темы группы: вид, даты, семестр; фильтр по семестру и предмету
        let mut codes: Vec<String> = Vec::new();
        let mut topics = Json::arr();
        let mut kinds: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut due: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for c in &codes_all {
            let t = j.topic(g, c);
            let kind = j.kind_of(g, c);
            let first = j.entries.iter().filter(|e| &e.group == g && &e.code == c && e.date > 0).map(|e| e.date).min().unwrap_or(0);
            let issued = if !t.issued.is_empty() { t.issued.clone() } else if first > 0 { super::core::ymd(first as i64 + tz) } else { String::new() };
            let semester = if !t.semester.is_empty() { t.semester.clone() } else { semester_of_date(&issued) };
            if !semester.is_empty() && !semesters.contains(&semester) {
                semesters.push(semester.clone());
            }
            if !t.subject.is_empty() && !subjects.contains(&t.subject) {
                subjects.push(t.subject.clone());
            }
            if (!f_sem.is_empty() && semester != f_sem) || (!f_sub.is_empty() && t.subject != f_sub) {
                continue;
            }
            let dl = parse_local(&t.deadline).map(|x| x - tz).unwrap_or(0);
            let graded = j.entries.iter().filter(|e| &e.group == g && &e.code == c && e.grade.is_some()).count();
            // долг: работа уже оценивалась у кого-то или срок прошёл
            let is_due = kind != "custom" && (graded > 0 || (dl > 0 && dl < now));
            topics.push(
                Json::obj()
                    .with("code", c.as_str())
                    .with("kind", kind.as_str())
                    .with("kind_set", t.kind.as_str())
                    .with("title", t.title.as_str())
                    .with("subject", t.subject.as_str())
                    .with("semester", semester.as_str())
                    .with("semester_set", t.semester.as_str())
                    .with("issued", issued.as_str())
                    .with("issued_set", t.issued.as_str())
                    .with("deadline", t.deadline.as_str())
                    .with("overdue", dl > 0 && dl < now)
                    .with("note", t.note.as_str())
                    .with("unit", t.unit.as_str())
                    .with("short", if t.short.is_empty() { super::journal::auto_short(c, &t.title) } else { t.short.clone() })
                    .with("short_set", t.short.as_str())
                    .with("moodle", t.moodle.as_str())
                    .with("mscale", t.mscale.as_str())
                    .with("graded", graded)
                    .with("due", is_due),
            );
            kinds.insert(c.clone(), kind);
            due.insert(c.clone(), is_due);
            codes.push(c.clone());
            if !all_codes.contains(c) {
                all_codes.push(c.clone());
            }
        }
        let roster = j.rosters.get(g).cloned().unwrap_or_default();
        let mut students: Vec<String> = roster.clone();
        let mut extra: Vec<String> = j.entries.iter().filter(|e| &e.group == g && !roster.contains(&e.student)).map(|e| e.student.clone()).collect();
        extra.sort_by_key(|s| natural_key(s));
        extra.dedup();
        students.extend(extra);
        let avgs: Vec<(String, Vec<String>)> = j.averages.get(g).cloned().unwrap_or_default();
        let mut rows = Json::arr();
        for stu in &students {
            let mut cells = Json::obj();
            let (mut sa, mut na, mut sw, mut nw, mut stt, mut nt, mut viol, mut missing, mut flags) = (0f64, 0, 0f64, 0, 0f64, 0, 0i64, 0i64, 0i64);
            let mut shown: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
            for c in &codes {
                let kind = kinds.get(c).map(|s| s.as_str()).unwrap_or("work");
                let e = j.entries.iter().find(|e| &e.group == g && &e.student == stu && &e.code == c);
                let raw = e.and_then(|e| e.grade);
                // тесты на 0–1,9 — это «2» (по настройке)
                let val = raw.map(|x| if kind == "test" && tests_min2 && x < 2.0 { 2.0 } else { x });
                if let Some(e) = e {
                    if !e.violation.is_empty() {
                        viol += 1;
                    }
                    if e.flag.contains("ai_text") {
                        flags += 1;
                    }
                    cells.set(
                        c,
                        Json::obj()
                            .with("grade", val)
                            .with("raw", raw)
                            .with("violation", e.violation.as_str())
                            .with("flag", e.flag.as_str())
                            .with("date", e.date)
                            .with("file", e.file.as_str())
                            .with("comment", e.comment.as_str())
                            .with("variant", e.variant.as_str()),
                    );
                }
                match (kind, val) {
                    ("custom", _) => {}
                    (k, Some(v)) => {
                        sa += v;
                        na += 1;
                        if k == "test" {
                            stt += v;
                            nt += 1;
                        } else {
                            sw += v;
                            nw += 1;
                        }
                        shown.insert(c.as_str(), v);
                    }
                    (_, None) => {
                        if due.get(c).copied().unwrap_or(false) {
                            missing += 1;
                        }
                    }
                }
            }
            let mut own = Json::obj();
            for (name, list) in &avgs {
                let vals: Vec<f64> = list.iter().filter_map(|c| shown.get(c.as_str()).copied()).collect();
                own.set(name, if vals.is_empty() { Json::Null } else { Json::Num(r2(vals.iter().sum::<f64>() / vals.len() as f64)) });
            }
            let avg = |s: f64, n: i32| if n > 0 { Json::Num(r2(s / n as f64)) } else { Json::Null };
            rows.push(
                Json::obj()
                    .with("name", stu.as_str())
                    .with("in_roster", roster.contains(stu))
                    .with("cells", cells)
                    .with("avg", avg(sa, na))
                    .with("avg_works", avg(sw, nw))
                    .with("avg_tests", avg(stt, nt))
                    .with("avg_own", own)
                    .with("violations", viol)
                    .with("flags", flags)
                    .with("missing", missing),
            );
        }
        let map_json = |src: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>| {
            let mut o = Json::obj();
            if let Some(m) = src.get(g) {
                for (k, v) in m {
                    o.set(k, v.as_str());
                }
            }
            o
        };
        let has_tests = kinds.values().any(|k| k == "test");
        let has_works = kinds.values().any(|k| k == "work");
        out.push(
            Json::obj()
                .with("name", g.as_str())
                .with("program", j.program_of(g))
                .with("in_registry", j.groups.iter().any(|x| &x.name == g))
                .with("roster", roster)
                .with("variants", map_json(&j.assigned))
                .with("teams", map_json(&j.teams))
                .with("codes", codes)
                .with("codes_all", codes_all)
                .with("topics", topics)
                .with("has_tests", has_tests)
                .with("has_works", has_works)
                .with("averages", Json::Arr(avgs.iter().map(|(n, c)| Json::obj().with("name", n.as_str()).with("codes", c.clone())).collect()))
                .with("students", rows),
        );
    }
    semesters.sort();
    all_codes.sort_by_key(|c| natural_key(c));
    Json::obj()
        .with("groups", out)
        .with("codes", all_codes)
        .with("semesters", semesters)
        .with("subjects", subjects)
        .with("filter", Json::obj().with("semester", f_sem.as_str()).with("subject", f_sub.as_str()))
        .with("opts", opts)
        .with("variant_is_number", j.variant_is_number)
        .with("entries", j.entries.len())
}

fn ctx_info_json(core: &Arc<Core>, prov: &Provider) -> Json {
    Json::Arr(
        grader::lmstudio_models_info(core, &prov.base_url)
            .into_iter()
            .map(|(id, loaded, max, is_loaded)| Json::obj().with("id", id).with("loaded_ctx", loaded).with("max_ctx", max).with("loaded", is_loaded))
            .collect(),
    )
}

/// Сохранить шрифт в папку данных/fonts (имя без пути, без повторов).
fn save_font(core: &Core, name: &str, data: &[u8]) -> Result<String, String> {
    let ext = super::core::ext_of(name);
    if !["ttf", "otf", "woff", "woff2"].contains(&ext.as_str()) || data.len() > 25_000_000 {
        return Err("Нужен файл шрифта: .ttf, .otf, .woff или .woff2 до 25 МБ".into());
    }
    let dir = core.data_dir.join("fonts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let clean: String = name.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let mut file = clean.clone();
    let mut k = 2;
    while dir.join(&file).exists() && std::fs::read(dir.join(&file)).map(|b| b != data).unwrap_or(true) {
        file = format!("{}_{}.{}", std::path::Path::new(&clean).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), k, ext);
        k += 1;
    }
    std::fs::write(dir.join(&file), data).map_err(|e| e.to_string())?;
    Ok(file)
}

/// Настроенное подключение к Moodle: (клиент веб-служб, id пользователя).
fn moodle_ws_of(core: &Arc<Core>) -> Result<(super::moodle_ws::Ws, i64), String> {
    let c = core.settings().get("moodle_ws").clone();
    let ws = super::moodle_ws::Ws::new(&c.get("url").str_or(""), &c.get("token").str_or(""));
    if ws.token.is_empty() {
        return Err("Moodle не подключён: Настройки → «Moodle напрямую»".into());
    }
    Ok((ws, c.get("userid").f64_or(0.0) as i64))
}
