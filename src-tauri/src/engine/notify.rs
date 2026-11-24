//! Уведомления Windows: какие события показывать, решает преподаватель в настройках
//! («Уведомления»). Ядро только сообщает о событии — `notify` { kind, title, body };
//! окно приложения (main.rs) показывает его, если оно включено.

use super::core::Core;
use super::json::Json;

/// Виды уведомлений: (ключ, название в настройках, включено по умолчанию).
pub const KINDS: &[(&str, &str, bool)] = &[
    ("done", "Проверка завершена", true),
    ("errors", "Ошибки при проверке", true),
    ("ai", "Работы ждут решения по признакам ИИ", true),
    ("long", "Готово долгое дело: скачана модель, установлена программа, составлены варианты", true),
    ("update", "Вышла новая версия приложения", true),
];

/// Показывать ли уведомление этого вида (без учёта того, открыто ли окно).
pub fn wanted(settings: &Json, kind: &str) -> bool {
    let n = settings.get("notify");
    if !n.get("enabled").bool_or(true) {
        return false;
    }
    if kind == "test" {
        return true;
    }
    let def = KINDS.iter().find(|k| k.0 == kind).map(|k| k.2).unwrap_or(false);
    n.get("kinds").get(kind).as_bool().unwrap_or(def)
}

/// Только когда окно свёрнуто или не в фокусе (по умолчанию — да).
pub fn only_background(settings: &Json) -> bool {
    settings.get("notify").get("only_background").bool_or(true)
}

pub fn send(core: &Core, kind: &str, title: &str, body: &str) {
    (core.emit)("notify", Json::obj().with("kind", kind).with("title", title).with("body", body).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter() {
        let s = super::super::json::parse(r#"{"notify":{"enabled":true,"kinds":{"done":false}}}"#).unwrap();
        assert!(!wanted(&s, "done"));
        assert!(wanted(&s, "errors"));
        assert!(!wanted(&s, "unknown"));
        let off = super::super::json::parse(r#"{"notify":{"enabled":false}}"#).unwrap();
        assert!(!wanted(&off, "errors"));
        assert!(only_background(&Json::obj()));
    }
}
