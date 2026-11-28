//! Ядро приложения «Проверка заданий». Написано на чистом Rust без внешних
//! зависимостей, поэтому собирается и тестируется отдельно от оболочки Tauri
//! (см. tools/devserver).

#![allow(dead_code)]

pub mod api;
pub mod assign;
pub mod core;
pub mod detect;
pub mod docx;
pub mod extras;
pub mod grader;
pub mod history;
pub mod imgfit;
pub mod lessons;
pub mod perf;
pub mod http;
pub mod journal;
pub mod json;
pub mod links;
pub mod llamacpp;
pub mod localsrv;
pub mod anon;
pub mod taskphoto;
pub mod notify;
pub mod transfer;
pub mod updater;
pub mod llm;
pub mod mcp;
pub mod moodle;
pub mod moodle_ws;
pub mod office;
pub mod report;
pub mod roster;
pub mod variants;
pub mod settings;
pub mod text;
pub mod zip;
