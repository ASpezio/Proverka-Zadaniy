//! Настройки по умолчанию и их слияние с сохранёнными.

use super::json::Json;

pub const DEFAULT_CRITERIA: &str = "Критерии оценивания (5-балльная шкала):
«5» — все задания выполнены полностью и самостоятельно, ответы верные; единичные опечатки и мелкие неточности классификации не снижают оценку.
«4» — работа выполнена полностью и самостоятельно, но есть 2–3 явные содержательные ошибки или один пункт не доделан.
«3» — выполнена примерно половина работы (около 50–60 %) или много ошибок в классификации / пропущены целые пункты.
«2» — выполнено менее трети работы, взят чужой вариант без выполнения своего, либо работа выполнена нейросетью (сработали скрытые ловушки, кодовые слова, вымышленные программы, следы запроса к ИИ).
Использование ИИ определяется по совокупности признаков: ответы, повторяющие скрытые приписки условия; вымышленные программы; кодовые слова; характерные фразы чат-бота. Одна случайная ошибка, совпавшая с ловушкой, сама по себе не доказывает использование ИИ.
Ограничения «максимум 3» (сдано позже срока, не указан номер варианта, отсутствие на паре) применяет приложение автоматически — оценивай содержание.";

pub fn defaults() -> Json {
    let prov = |id: &str, kind: &str, name: &str, url: &str| {
        Json::obj()
            .with("id", id)
            .with("kind", kind)
            .with("name", name)
            .with("base_url", url)
            .with("api_key", "")
            .with("model", "")
            .with("temperature", 0.1)
            .with("max_tokens", 3000)
            .with("num_ctx", 8192)
            .with("structured", true)
            .with("timeout_s", 600)
            .with("local", true)
    };
    let cap = |pattern: &str, max: i64, label: &str| Json::obj().with("pattern", pattern).with("max", max).with("label", label).with("enabled", true).with("mode", "max");
    Json::obj()
        .with(
            "providers",
            Json::Arr(vec![
                prov("lmstudio", "lmstudio", "LM Studio", "http://localhost:1234/v1"),
                prov("ollama", "ollama", "Ollama", "http://localhost:11434"),
                prov("llamacpp", "llamacpp", "llama.cpp (llama-server)", "http://localhost:8080/v1"),
            ]),
        )
        .with("active_provider", "lmstudio")
        .with("mode", "direct")
        .with("concurrency", 1)
        // почти одинаковые работы получают разбор уже проверенной (без нейросети)
        .with("reuse_copies", true)
        .with("max_work_chars", 16000)
        // открывать ссылки из работ студентов (Google Документы, Диски, GitHub, сайты)
        .with("links", Json::obj().with("enabled", false).with("max", 15))
        // картинки внутри Word/PowerPoint/Excel: описывает модель с поддержкой изображений
        .with("media", Json::obj().with("images", true).with("max_images", 20).with("max_side", 1600))
        .with(
            "penalties",
            Json::obj()
                .with("enabled", true)
                .with("ai_markers", true)
                .with("ai_model", true)
                .with("plagiarism", true)
                .with("ai_confidence", 0.7)
                .with("ai_multi", true)
                .with("ai_multi_count", 2)
                .with("injection", true)
                .with("decide", "ask")
                .with("plagiarism_threshold", 0.6)
                // сколько одинаковых работ можно зачесть; если больше — «2» всем
                // ("all") или всем, кроме первых сданных ("first")
                .with("plagiarism_allowed", 1)
                .with("plagiarism_over", "all"),
        )
        .with(
            "caps",
            Json::Arr(vec![
                cap("позже срок|после срок|опоздан|просроч|не в срок", 3, "Сдано позже срока — максимум «3»"),
                cap("не было на паре|не был на паре|отсутствовал", 3, "Не было на паре — максимум «3»"),
                cap("не написан вариант|не указан вариант|без варианта", 3, "Не указан номер варианта — максимум «3»"),
            ]),
        )
        .with("variant_missing_cap", 0)
        .with(
            "rules",
            Json::obj()
                .with("min_words", Json::obj().with("enabled", false).with("words", 150).with("max", 3))
                .with("screenshots", Json::obj().with("enabled", false).with("max", 4))
                .with("wrong_variant", Json::obj().with("enabled", false).with("max", 2))
                // условие требует файлы (например, docx и xlsx), а одного нет
                .with("missing_files", Json::obj().with("enabled", true).with("max", 3))
                // Excel: значения без формул, где их требует условие; нет диаграммы нужного вида
                .with("excel_checks", Json::obj().with("enabled", true).with("max", 4)),
        )
        .with("completeness_rules", true)
        .with("criteria", DEFAULT_CRITERIA)
        .with("custom_markers", Json::arr())
        .with("extra_instructions", "")
        .with("discipline", "")
        .with("group_name", "")
        .with("teacher", "")
        .with(
            "double_check",
            Json::obj().with("enabled", false).with("provider", "").with("model", "").with("checkers", Json::arr()).with("policy", "primary").with("max_diff", 2),
        )
        .with("journal_auto", false)
        .with("journal_manual", true)
        // журнал: тесты 0–1,9 → «2», какие столбцы выгружать
        .with("journal_opts", Json::obj().with("tests_min2", false))
        // предметы (дисциплины) и предмет каждого набора заданий
        .with("subjects", Json::arr())
        .with("set_subjects", Json::obj())
        // рубрики по наборам заданий: {id набора: {items: [{name, max, desc}], scale: {"5": 85, "4": 70, "3": 50}}}
        .with("rubrics", Json::obj())
        // архив работ прошлых лет: класть проверенные работы, сравнивать с той же работой (или со всеми)
        .with("archive", Json::obj().with("enabled", true).with("all_codes", false))
        // напоминания о сроках из календаря журнала: за сколько дней
        // копия в свою папку (Яндекс.Диск, флешка): файл экспорта раз в день
        .with("backup", Json::obj().with("dir", "").with("keep", 10).with("files", false))
        .with("reminders", Json::obj().with("enabled", true).with("days", 2).with("sent", Json::obj()))
        // CSV: кодировка (utf8bom — понимают и Moodle, и Excel; utf8; cp1251) и разделитель
        .with("csv_encoding", "utf8bom")
        .with("csv_sep", ";")
        // прямое подключение к Moodle (веб-службы): адрес сайта и ключ; пароль не хранится
        .with("moodle_ws", Json::obj().with("url", "").with("token", "").with("site", "").with("user", "").with("userid", 0))
        .with("half_grades", true)
        .with("tray", Json::obj().with("close_to_tray", false).with("minimize_to_tray", false))
        .with("llama", Json::obj().with("bin_dir", "").with("models_dir", "").with("selected", Json::arr()).with("ctx", 16384).with("gpu", "gpu").with("kv_q8", true).with("extra", ""))
        .with("programs", vec!["Школа (1–4 класс)".to_string(), "Школа (5–9 класс)".into(), "Школа (10–11 класс)".into(), "СПО-9".into(), "СПО-11".into(), "Специалитет".into(), "Бакалавриат".into(), "Магистратура".into()])
        .with("theme", "paper")
        .with("cache_results", true)
        // длинные работы: как сжимать, если не помещаются в контекст модели
        .with("compress", "dense")
        // облачным нейросетям — без ФИО, почты и телефонов (152-ФЗ)
        .with("privacy", Json::obj().with("anonymize", true))
        // уведомления Windows: какие показывать и только ли когда окно в фоне
        .with("notify", Json::obj().with("enabled", true).with("only_background", true).with("kinds", Json::obj()))
        // проверка новой версии на GitHub: start | weekly | manual
        .with("updates", Json::obj().with("mode", "start").with("last_check", 0).with("skip", ""))
        .with("animations", true)
}

/// Накладывает сохранённые значения на значения по умолчанию (новые ключи не теряются).
pub fn merge(base: &Json, saved: &Json) -> Json {
    match (base, saved) {
        (Json::Obj(b), Json::Obj(s)) => {
            let mut out = Json::Obj(b.clone());
            for (k, v) in s {
                let merged = match b.iter().find(|x| &x.0 == k) {
                    Some((_, bv)) if matches!(bv, Json::Obj(_)) => merge(bv, v),
                    _ => v.clone(),
                };
                out.set(k, merged);
            }
            out
        }
        (_, s) if !s.is_null() => s.clone(),
        (b, _) => b.clone(),
    }
}
