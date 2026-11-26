# AGENTS.md: памятка для ИИ-ассистентов, работающих с проектом

Пользовательская инструкция находится в `ИНСТРУКЦИЯ.md`. Здесь описано, как устроен код и как его безопасно менять.

## Назначение
Настольное приложение (Windows, Tauri 2 + Rust) для автоматической проверки практических работ студентов любых направлений (СПО-9, СПО-11, специалитет, бакалавриат, магистратура) локальной или облачной LLM. Первый сценарий: ПР2 «Программное обеспечение информационных технологий», СПО-11. В заданиях есть скрытые анти-ИИ ловушки (белый текст 1 пт), приложение ищет их следы в работах и умеет автоматически ставить «2» за ИИ и списывание.

## Архитектура
```
ui/ (vanilla JS)  ──invoke("api",{method,payload})──►  src-tauri/src/main.rs
                                                        │  spawn_blocking
                                                        ▼
                                           engine::api::dispatch(method, json)
                                                        │
       ┌──────────────── engine::core::Core (состояние, правила, сессия) ────────────────┐
       │  docx.rs   zip.rs   text.rs   assign.rs   detect.rs   report.rs   settings.rs    │
       │  grader.rs ──► mcp::McpClient ──JSON-RPC (in-process)──► mcp::McpServer ──► Core │
       │      └──► llm.rs (Ollama / OpenAI-совм. / Anthropic) ──► http::HttpClient        │
       └───────────────────────────────────────────────────────────────────────────────────┘
События ядро → UI: "progress", "run_state", "log", "mcp" (payload — JSON-строка).
```

### Ключевой инвариант: ядро без внешних зависимостей
`src-tauri/src/engine/*` использует **только std**: свой JSON (`json.rs`), DEFLATE и ZIP (`zip.rs`), XML-токенизатор и чтение DOCX/ODT/RTF/DOC (`docx.rs`), генерацию DOCX/XLSX (`report.rs`). Благодаря этому ядро собирается и тестируется без Tauri и без доступа к crates.io:
```
cd tools/devserver
cargo test                         # юнит-тесты ядра
cargo run --release -- check "<папка_заданий>" "<папка_работ>"   # разбор + автопроверки
cargo run --release -- serve 8765 ../../ui                       # UI в браузере, API по HTTP
cargo run --release -- mcp                                       # MCP по stdio
python ../mock_llm.py 18080        # имитация LLM (OpenAI-совместимый /v1 и Ollama /api)
```
Внешние крейты есть **только** в `main.rs`: `tauri`, `tauri-plugin-dialog`, `tauri-plugin-single-instance`, `ureq` (HTTPS). Не тащите их в `engine`.

## Файлы ядра
| Файл | Что делает |
|---|---|
| `json.rs` | `Json` enum, `parse`, `to_string/pretty`, `extract_object` (достаёт JSON из ответа модели: ```json, `<think>`, висячие запятые, обрывы) |
| `zip.rs` | чтение ZIP (stored/deflate, собственный inflate), запись ZIP (stored, CRC32) |
| `docx.rs` | извлечение текста (диспетчер `extract`: docx/odt/rtf/doc/html/txt, xlsx/pptx → `office.rs`, фото → `needs_images`, pdf → `needs_pdf`); скрытый текст оборачивается маркерами `\u{E000}…\u{E001}`; `visible_of`, `hidden_of`; таблицы → строки `a \| b \| c` |
| `text.rs` | CP1251, нормализация, `contains_phrase`, `find_variant` («Вариант 12», «В20», «Варинт»), `find_variant_word` (только словом: «Вариант 5», «5 вариант» — для всего текста), `find_variant_in_name` (имя файла: ещё «14В», «11 В», «27ВАР», «ВР11»), `find_work_code` («ПР2») |
| `roster.rs` | разбор списка группы (`parse`: нумерация, таблицы «ФИО \| Вариант», строки Excel «Строка N — B: …», «— вариант 3»), `SYSTEM_VISION_ROSTER` для фото списка |
| `assign.rs` | наборы заданий (по коду ПРn): деление на варианты по строке «Вариант N», ключи ответов, строка «ИИ», вычисление слов-маркеров |
| `detect.rs` | детерминированные улики (`Evidence` strong/medium/weak), определение варианта по содержанию, антиплагиат (6-словные шинглы) |
| `core.rs` | `Core`: загрузка файлов, сессия (`%APPDATA%\proverka\session.json`; старая папка `ru.pspu.proverka` переносится `migrate_data_dir`, пути в сессии переписываются в `restore_session`), `finalize()` — правила оценки поверх ответа модели |
| `mcp.rs` | MCP-сервер (initialize, tools/*, resources/*, prompts/*), in-process клиент с трассировкой в UI, stdio-транспорт |
| `grader.rs` | очередь и потоки, режимы `direct` и `agent`, системные промпты, сбор контекста через MCP |
| `llm.rs` | провайдеры: `ollama` (/api/chat), `lmstudio`/`llamacpp`/`openai` (/chat/completions), `anthropic` (/v1/messages); tool calling; JSON-schema |
| `report.rs` | ведомость DOCX (альбомная таблица), XLSX, CSV |
| `journal.rs` | журнал семестра (`journal.json`): реестр групп `{name, program, note}`, списки групп, `assigned` (группа → ФИО → назначенный вариант), записи оценок; `student_for` (фамилия в имени файла → студент с назначенным вариантом → номер в списке, если варианты не розданы) |
| `extras.rs` | вопросы для устной защиты и отзыв студенту через `grader::ask_once` |
| `localsrv.rs` | LM Studio и Ollama «в один клик»: поиск (`lms`, `LM Studio.exe`, `ollama.exe`, PATH), установка через winget, `server start/stop`, `ollama serve`, `ollama pull` / `lms get` с ходом (событие `local_progress`), `ensure_provider` |
| `anon.rs` | обезличивание для облака (152-ФЗ): `Anon::new(ФИО)`, `apply` (фамилия в любом падеже и латиницей, имя/отчество рядом с фамилией, инициалы, почта, телефоны → «Студент N», `[почта]`, `[телефон]`), `restore` в ответе. Включается в `grader::chat_cancellable` для не-локальных провайдеров (`settings.privacy.anonymize`) |
| `taskphoto.rs` | задания с фото (`SYSTEM_VISION_TASK`, разделитель «=== ОТВЕТЫ ==="), эталон нейросетью (`SYSTEM_SOLVE`), вопросы перед генерацией (`SYSTEM_PLAN`), сохранение в `data_dir/tasks_text` и `add_tasks` с `TaskMeta` |
| `notify.rs` | уведомления Windows: `KINDS`, `wanted(settings, kind)`, `send` → событие `notify`; показывает `main.rs` (`tauri-plugin-notification`) |
| `transfer.rs` | экспорт/импорт всего (`.proverka` = zip + `manifest.json`): части `settings/keys/session/journal/results/looks/files`; внешние файлы → `files/NNNN/<папка>/<имя>`, внутренние → `data/…`; при импорте пути в `session.json` переписываются |
| `updater.rs` | обновления с GitHub Releases (`REPO`, ассет `ProverkaZadaniy.exe`): `check`, `newer`, `due` (`settings.updates.mode`: start/weekly/manual), `download` + скрипт `apply.ps1` (UTF-8 BOM), `apply_and_exit` |
| `api.rs` | все методы для UI (см. `dispatch`): файлы (`add_paths` с `scope`/`group`, `remove`, `clear`, `set_doc_kind`, `set_task_scope`, `set_work_assignment`, `set_work_group`), группы (`group_save`, `group_delete`, `programs_save`), модели (`list_models`, `provider_info`, `lmstudio_reload`, `test_provider`, `detect_local`), проверка (`start_check`, `cancel_check`, `set_override`), ведомость (`report_preview`/`export` с фильтром `group`), `oral`, `feedback`, `export_feedback`, `export_oral`, журнал (`journal*`), MCP (`mcp_info`, `mcp_call`), `prompt_preview`, `open_path` |

### Excel, PowerPoint, фото и сканы
* `office.rs`: `from_xlsx` (sharedStrings, листы по workbook.xml + rels, «Строка N — A: … | D: =B2*C2 → 150», скрытые листы, диаграммы `xl/charts`, фильтры/УФ/закрепление) и `from_pptx` (порядок слайдов по presentation.xml, текст/таблицы, заметки, рисунки, переходы, анимации, гиперссылки, скрытые слайды). Без `###`/`**` — иначе `detect.rs` примет это за Markdown из чата; ссылки на ячейки в формулах `text::find_variant` не считает вариантом.
* Изображения: `docx::IMAGE_EXT`, `Extracted.needs_images`. `Work.pages: Vec<Page{src,file,hash,size,ready}>`, `Work.sources` (присоединённые файлы, сохраняются в сессии), `transcript`/`transcribed`. `core::group_pages` при добавлении папки объединяет фото одной работы; `Core::attach_sources`, API `merge_works {ids}`. Word без текста с фото → `media_pages`.
* Подготовка в UI (`prepareFiles` в app.js, вызывается при старте, после добавления, после Moodle и перед `start_check`): PDF → `set_pdf_text` (или `failed`), скан (мало текста) → страницы в JPEG → `set_pages`; большие фото (`prep`) → уменьшение до 2000 px → `set_page_image`. Файлы — в `<data>/pages`.
* Распознавание: `grader::transcribe` перед проверкой, `Msg::UserImages` (OpenAI `image_url` data URL, Ollama `images`, Anthropic `image`), кэш `<data>/pages/<hash>.txt`, `core::apply_transcript`. Нет vision → `vision_error`. llama.cpp: `llamacpp::find_mmproj` добавляет `--mmproj`.
* `tools/mock_llm.py`: отвечает на изображения; `MOCK_MODE=novision` — ошибка «Model does not support images».

### Ссылки в работах (`links.rs`)
`find_in_file` (гиперссылки из `*.rels` OOXML, `text:a` ODT, `<a href>` HTML) → `Work.urls`; `Work::all_urls` добавляет адреса из текста (`find_in_text`). `settings.links {enabled, max}`. `grader::fetch_links` перед проверкой → `Work.link_docs` (не сохраняются) и `Outcome.links` (сводка). MCP `get_work_links` (в `AGENT_TOOLS`), в прямом режиме — раздел «МАТЕРИАЛЫ ПО ССЫЛКАМ» (доля бюджета 0.2, `detect::neutralize`). `submit` добавляет в `review` неоткрытые ссылки и инъекции. Скачивание — `HttpClient::get_raw` (без авто-redirect; в `main.rs` на ureq), каждый шаг переадресации проверяет `links::allowed` (запрет локальной сети; для тестов `PROVERKA_ALLOW_LOCAL_LINKS=1`). Особые случаи: Google Docs/Sheets/Slides → export, Google Drive → uc?export=download (+confirm), Яндекс Диск → cloud-api public download, GitHub → API (repo, tree, readme) / raw. `open_url` открывает ссылку через rundll32 (не cmd).

### Ссылки: защита от роботов
`links::get` — куки между переадресациями (`Jar`), пауза 1,2 с на сайт (`polite`), повтор после 429, обнаружение круга. Если сайт не пустил (401/403/429, круг, `looks_blocked`) — `HttpClient::render_page`: в `main.rs` окно WebView2 за экраном (`CAPTURE_JS` отдаёт `innerText` переходом на `https://proverka-capture.invalid/?d=…`, `on_navigation` его перехватывает и не пускает в локальную сеть). `fetch_cached` — кэш ссылок на сеанс. `focus_text` срезает меню перед заголовком.

### Объекты Office (`office.rs`)
`chart_text` (тип, заголовок, оси, ряды из кэша или ссылка на ячейки; chartEx), `smartart_text` (дерево по `cxn parOf`), `drawing_texts`/`comments_text` для Excel, `part_rels`. Word: `parse_document_xml_with(…, embed)` вставляет диаграммы и SmartArt на место, пропускает `mc:Fallback`, читает OMML (`m:t`, дроби, степени, корни) и `v:textpath`. PowerPoint: `slide_text` возвращает `Embeds` (C/D/V). Картинки внутри документов — `Core::media_pages` (word/ppt/xl/media, Pictures) → `Work.pages`; `Work::embedded_pictures`; в `grader::transcribe` они необязательные (`settings.media {images, max_images}`), ошибка модели не останавливает проверку; `compute_plagiarism` их описания не сравнивает.

### Обязательные файлы
`detect::required_files(task)` — виды файлов из фраз про сдачу (группы «или»), `file_kind`/`kind_label`. `Core::scan` заполняет `Scan.required_files/submitted_files/missing_files` (файлы работы, `Work.extra_kinds` — вложенные объекты `embedded_kinds` и нечитаемые файлы из папки Moodle, ссылки Google). Правило `rules.missing_files {enabled, max}` в `finalize`; в промпте — состав работы и предупреждение. `text::clip_parts` делит бюджет между файлами («=== Файл «…» ===»). `group_pages` объединяет документы с одинаковым именем. `refinalize_all` — при запуске и при смене `rules`/`penalties`/`caps`.

### Очередь проверки
Очередь общая — `Core.queue` (её читают потоки `grader::run`), `Core.run_total` — сколько работ в проверке, `Core.dropped` — снятые во время проверки. API `enqueue {ids}` и `dequeue {ids}` работают во время проверки. `grader::should_stop` (общая отмена или снята текущая работа потока — `CURRENT` thread_local) проверяется вместо `core.cancel` внутри проверки работы. Excel-ведомость: порядок частей листа по схеме OOXML (`autoFilter` до `mergeCells`).

### Проверка Excel, токены, время
`office::xlsx_facts` → `Work.xlsx` (формулы, числа без формул в «формульных» столбцах, итоговые строки, виды диаграмм); `detect::task_needs_formulas`, `detect::task_chart`; `Core::excel_checks` → `Scan.excel_findings/excel_ok`; правило `rules.excel_checks {enabled, max}`. Токены: `store_tokens` шлёт событие `tokens` (по работе и за проверку, `Core.run_tokens_*`), итог — в `run_state`. `Outcome.checked_at` ставится по окончании проверки работы; `report::fmt_dt` (часовой пояс `TZ_MIN` присылает интерфейс в `tz`) — «Проверено: …» в ведомости. Во время проверки `set_settings` разрешён для `theme`, `sound`, `sound_volume`, `animations`, `tray`.

### Вариант работы
`detect::scan_work`: `variant_in_name` (`find_variant_in_name`), `variant_in_text` (первые 40 строк — любые формы, дальше — только словом). При расхождении главнее имя файла (`Scan.variant_notes`). `Scan.variant_source`: `name`/`text`/`single`/`roster`/`content`/"". `Core::scan` подставляет вариант из списка группы (`Core::roster_variant`), если номера нет нигде, и пишет расхождение со списком. При `content` условие подбирается по похожести (`best_match_variant`) только для проверки — наружу (`variant_shown()`: ведомость, журнал, `work_json.variant`) такой номер не идёт; `work_json.variant_checked` — по какому варианту проверялось. `variant_missing_cap` — только если номера нет ни в имени, ни в тексте. `variant_notes` попадают в `reasons`.

### Списки групп и раздача вариантов
API: `roster_import {path | image(base64)+mime}` (Excel/Word/текст через `docx::extract`, фото — `grader::vision_text` с `SYSTEM_VISION_ROSTER`), `roster_parse {text}`, `journal_roster {group, names, variants:{ФИО: вариант}, variant_is_number}` (после сохранения — `refinalize_all`). `journal_view` отдаёт `groups[].variants`. Раздача (по порядку, по кругу, случайно поровну) — в UI (`editRoster`).

### Работа в парах
`Journal.teams` (группа → ФИО → номер пары), `team_of`. `Core::authors(ws, w)` = `main_student` (бывший `student_of`) + фамилии одногруппников из имени файла и титульных строк (`roster::mentioned`: уникальные слова ФИО от 4 букв, кириллица и `roster::translit`) + напарники по `teams`. `student_of` возвращает их через « и ». Журнал (`journal_add_current`) и `moodle_grades` пишут оценку каждому автору. `compute_plagiarism` выкидывает пары работ с общими авторами (если хоть у одной авторов больше одного) в `Workspace.team_dups` → `work_json.team_twin`. `student_for`: вариант, назначенный одной паре, тоже определяет студента. API `journal_student {group, student, action: delete|rename, to}` (`Journal::delete_student`, `rename_student` — объединение оценок).

### Работы на фото
`add_photo_work {paths, group, student}` — первая картинка = работа, остальные `attach_sources`. `SYSTEM_VISION` просит восстанавливать неразборчивый почерк по смыслу с пометкой `[?слово]`; `grader::transcribe` добавляет к запросу условие варианта (из имени файла или единственного) как подсказку. Системные промпты проверки знают про `[?…]` и не снижают оценку за почерк.

### Оформление
`settings.look_style` (id из `STYLES`: notebook, glass, fluent, swiss, soft, brutal, news, terminal, neon) → `data-style` на `<html>`; правила стилей в конце styles.css, все вида `[data-style="x"] .класс` — поэтому они же работают в живых превью (`.pv[data-style]` в `stylePreview`). У brutal/news/terminal/neon свои цвета (`[data-style="x"][data-style]` — специфичность выше схемы; CSS схем вставляется в начало `<head>`), `palette:false`, тёмность берётся из стиля. `settings.theme` (id из `THEMES` в app.js или `auto`), `font_ui`, `font_head`, `font_size`. Тема задаётся 5 опорными цветами, остальные переменные выводит `themeVars()`; CSS тем генерирует `installThemeCss()` (кроме `paper` и `uv`, они в styles.css). Тёмные темы ставят `data-dark` — все тёмные поправки в CSS через `[data-dark]`. Шрифты — переменные `--font-ui`, `--font-head`, `--font-mono`, `--fs`; системные шрифты проверяются `fontAvailable()` (сравнение ширины текста). Окно — `openLook()`. Эти ключи разрешено менять во время проверки.

### Сборка, версия
`tools/build.ps1` (-Notify из `Собрать.bat`): номер сборки в `tools/build_number.txt` (растёт после успешной сборки), `src-tauri/build_info.txt` = «v1.N.0 · сборка N · ревизия · дата», `build.rs` передаёт его в `PROVERKA_BUILD` → `app_info.build`. В конце — окно «Сборка готова» / ошибка, перед копированием — закрытие запущенного приложения. Обрыв JSON-ответа: `grader::budget_boost` удваивает лимит ответа до 3 раз.

### Несколько моделей-проверяющих
`settings.double_check.checkers = [{provider, model, num_ctx, no_think?}]` (старый формат `provider`+`model` читается `grader::checkers_of`). `grader::checker_providers` собирает провайдеров (id `<provider>#<n>`), контекст каждого определяется перед проверкой и сохраняется (`remember_checker_ctx`). Мнения копятся в `llm.opinions` (первое дублируется в `llm.second_opinion`), объединение — `Core::attach_second_opinion` (политики primary/avg/median/min/max → `llm.merged_grade` → `refinalize`).

### Moodle (`moodle.rs`)
`parse_name` разбирает «ФИО_id_assignsubmission_…», `unzip` (с защитой от «..», время файлов из архива), `collect` выбирает главный файл студента, остальные (`extra`) `moodle_import` присоединяет через `attach_sources`; повторный импорт того же ответа пропускается (ФИО + хэш + имя). Присоединённые PDF ждут интерфейс: `Work.pending_pdf` → `pdf_sources` в JSON → `read_file_b64 {source}` → `set_source_text {i, text, images|failed}`; ФИО хранится в `Work.student` (сохраняется в сессии; `add_works` сам распознаёт имена Moodle). API: `moodle_import {path, group, roster}`, `moodle_grades {path, group, feedback}` — заполняет CSV-таблицу оценок Moodle (`parse_csv`/`write_csv`, сопоставление ФИО `same_person`).

### llama.cpp в один клик (`llamacpp.rs`)
`find_bin` (settings.llama.bin_dir → папка данных → PATH), `install` (GitHub releases/latest: CPU-сборка + CUDA/Vulkan + cudart, `HttpClient::download`, распаковка через `tar` Windows), `scan_models` (gguf без mmproj и без хвостовых частей шардов), `start` (по llama-server на модель, порты с 8090, ожидание `/health`, журналы в `<data>/llama.cpp/logs`), `register` (провайдеры `llamacpp-auto-N`, первая — активная, остальные — `checkers`), `stop_all` (вызывается и при выходе из приложения в `main.rs`). События интерфейсу — `llama`.

### Группы и «для кого»
* `TaskDoc.scope` / `AssignmentSet.scope`: «» (для всех), направление или группа. Ключ набора — `assign::set_key(code, scope)` → `"ПР2"` или `"ПР2@СПО-11"`; это `AssignmentSet.id`, его хранит `Work.assignment` при ручном выборе.
* `Core::set_for_work`: ручной выбор → кандидаты по `set_fits` (группа > направление > для всех) → код ПРn из имени → единственный → по содержанию. Если подходящих нет, берётся любой, а `work_json.assignment_foreign = true` (в UI жёлтая пометка).
* Направление группы — `Journal::program_of(group)`; список направлений — `settings.programs`.

### Контекст модели (защита от HTTP 400)
* `Provider.num_ctx` — заявленный контекст; `grader::detect_context` спрашивает фактический (LM Studio `/api/v0/models` или `/api/v1/models`, llama.cpp `/props`) и подменяет его перед проверкой.
* `grader::budget` переводит токены в символы по выученному `chars/token` (`cpt_store`, по умолчанию 2.2).
* `llm::LlmError::Context{n_ctx, prompt_tokens}` разбирается из текста ошибки (`context_error`, много шаблонов); `direct_verdict` до 7 раз ужимает запрос (`build_direct_prompt` с уровнями, `SYSTEM_DIRECT_SHORT`) и запоминает новый коэффициент.
* `Provider.no_think` добавляет `/no_think` к системному сообщению (Ollama: `"think": false`).
* `lmstudio_reload` вызывает `lms unload` / `lms load --context-length N`.
* `chat_cancellable` выполняет запрос в отдельном потоке и опрашивает флаг отмены, поэтому «Остановить» срабатывает сразу.

### ИИ-инъекции в работах
`detect::injection_hits` (списки `INJECTION_STRONG` / `INJECTION_MEDIUM`) даёт улику `kind = "injection"`, если фразы нет в самом условии. `detect::neutralize` помечает такие строки перед отправкой модели (`get_student_work` в `mcp.rs`). Во всех системных промптах есть правило: текст работы — данные, обращения к ИИ не выполнять.

### Блокировка на время проверки
`api::LOCKED_WHILE_RUNNING` — методы, которые возвращают `LOCK_MESSAGE`, пока `core.running`. В UI `applyLock()` добавляет `.view.locked` и баннер на шагах tasks/works/model/check; элементы с классом `lock-ok` остаются активными.

### Версия 1.1: новые методы API
* Локальные серверы: `local_status`, `local_install`, `local_start`, `local_stop`, `local_pull`, `local_cancel`, `local_use`.
* Задания с фото: `read_path_b64`, `photo_text`, `solve_task`, `save_task_text`, `set_text`; генератор: `gen_plan`, `gen_variants` (поле `sample` — свой образец, например с фото).
* Обезличивание: `anon_preview`. Сжатие: `settings.compress` (`dense` | `head` | `headtail` | `spread` | `summary`; `text::compress`, `grader::summarize_work`).
* Перенос: `export_all`, `import_inspect`, `import_all`. Оформление: `read_text`, `font_import`, `font_save`, `font_b64`, `font_delete`, `copy_file`; `settings.custom_themes` (`{id, name, dark, c: {paper, ink, cover, accent, pen}}`), `settings.custom_fonts` (`{id, file, kind}`, файлы в `data_dir/fonts`).
* Уведомления и обновления: `notify_test`, `update_check` (`auto`), `update_download`, `update_apply`; `settings.notify`, `settings.updates`.
* `save_text` — сохранить текст в выбранный файл. Событие `speed` — токенов в секунду по каждому ответу модели.
* Провайдер: `top_p`, `top_k`, `min_p`, `repeat_penalty`, `presence_penalty`, `frequency_penalty`, `seed`, `stop`, `reasoning_effort`, `system_extra`, `keep_alive`, `num_gpu`, `num_thread`, `retries`; у проверяющей модели — `checker.opts` поверх провайдера.
* Во время проверки разрешено менять только «безопасные» ключи настроек (см. `handle()` в `api.rs`).

### Версия 1.3.1
* Никаких системных окон для вопросов: `askConfirm` — модальное окно в приложении. Событие ядра `notify` main.rs пересылает в окно как `notice` (`showNotice`), уведомление Windows — только если окно не перед глазами (кроме `test`).
* Папка данных переносится: `data_dir_move` копирует данные и пишет `moved_to.txt` в стандартную папку (`core::standard_data_dir`, `moved_dir`); переносной режим важнее.
* Журнал: порядок строк — только в интерфейсе (`JSORT`, `sortStudents`, localStorage `jSort`); раздача вариантов «по алфавиту».
* Свои стили: `settings.custom_styles` [{id, name, base, opts}], CSS — `customStyleCss` под `:root[data-cstyle]` и `.pv[data-cstyle]`, `data-skin`/`data-style` — стиль-основа.

### Версия 1.3: Moodle напрямую, рубрика, история
* `moodle_ws.rs` — REST веб-служб Moodle (`Ws::call`, `get_token` через login/token.php, `grade_rows` → строки как у экспорта журнала оценок, `save_grade`). API: `moodle_ws_connect|disconnect|courses|course|fetch|grades|push`. Ключ — `settings.moodle_ws.token` (в экспорт без «ключей» не попадает). `moodle_ws_fetch` складывает ответы в папку как у архива «Скачать все ответы» и вызывает `moodle_import`; `moodle_ws_grades` подаёт строки в `moodle_import_grades` (`p.rows`, `p.scales`).
* Тема журнала помнит заголовок и шкалу Moodle (`Topic.moodle`, `mscale`) и короткое имя (`short`, `journal::auto_short`); `moodle::export_gradebook_named` выгружает под ними.
* Рубрика: `core::Rubric` (`settings.rubrics[set.id]`), `Rubric::apply` в `refinalize` и `mcp::submit` — оценка по сумме баллов (`llm.rubric*`, `model_grade`); текст для модели — `get_grading_criteria` с `work_id`; критерии входят в отпечаток кэша. API `rubric_set` пересчитывает проверенные работы.
* `history.rs`: сверка (`calibration.json`, `cal_add` в `journal_add_current`, `cal_summary` → API `calibration`) и архив прошлых лет (`archive/<id>.txt`: мета + шинглы; `arc_matches` в `compute_plagiarism` → `Workspace.archive_hits`, работа: `archive_hits`). API `archive_summary|list|add_paths|delete`.
* Отчёты: `journal_debtors`, `journal_student_report`, `defense_plan`, `defense_candidates`; напоминания `reminders_check` (вид уведомления `deadline`, отправленные — `settings.reminders.sent`).
* Копии в свою папку: `Core::backup_external` (`settings.backup.dir`), из `backup_daily`/`backup_now`. Переносной режим: `core::portable_dir()` (рядом с exe `portable.txt` или `Данные/settings.json`), `rewrite_moved_dir` + `data_dir.txt` чинят пути после переезда; API `portable_info|portable_enable`.
* CI: задание `ui` (Linux) — тесты ядра и `tools/ui_smoke.js` (devserver + Chromium, вымышленные данные); `windows` — сборка, необязательная подпись (`SIGN_PFX_BASE64`/`SIGN_PFX_PASSWORD`), установщик и его тихая установка/удаление. Моки: `tools/mock_llm.py` (понимает рубрику), `tools/mock_moodle.py`.

### Версия 1.2: журнал, темы, Moodle
* `journal.json` (version 2): `entries` + `topics` (группа → шифр → `Topic {kind: work|test|custom|"", title, subject, semester, issued, deadline, note, unit}`) + `averages` (группа → [(название, [шифры])]). Пустой `kind` — по названию (`journal::auto_kind`: «тест», test, quiz, опрос). `custom` не идёт в средние и долги.
* Долг: тема не `custom` и (у кого-то уже есть оценка или прошёл срок). Семестр по дате выдачи (`semester_of_date`: сен–янв — 1-й, фев–авг — 2-й).
* Запись в журнал только вручную: `journal_add_current`; `settings.journal_auto` по умолчанию `false`.
* Срок из календаря работает как срок работы: `Core::deadline_of(ws, w)` (используется в `refinalize` и `mcp::submit_grade`).
* API: `journal_view {f_semester, f_subject}` (фильтры называются так, потому что `subject` занят полезной нагрузкой импорта и тем), `journal_topic`, `journal_fill {group, codes, grade}`, `journal_averages`, `journal_calendar`, `set_subject {set, subject}`, `group_delete {name, entries}`, `journal_export {cols}`.
* Moodle: `moodle::column_info` разбирает заголовки «Задание: …»/«Тест: …» и вид в последних скобках (Значение, Проценты, Буква, Отзыв); `import_grades_as(rows, code, prefer)`; `export_gradebook_full(…, feedback, sep)`; `journal::mentions_ai` помечает отзыв (`Entry.flag = "ai_text"`).
* CSV: `settings.csv_encoding` (`utf8bom` | `utf8` | `cp1251`, `moodle::encode_csv`), `settings.csv_sep`.
* Уведомления: `main.rs::register_toast_id` пишет AUMID (`identifier`) в `HKCU\Software\Classes\AppUserModelId` — без этого Windows не показывает уведомления у неустановленного exe.
* Файлы удаляются вместе с работами/заданиями/группами: `Core::cleanup_files()`.

### Репозиторий и релизы
* GitHub: `ASpezio/Proverka-Zadaniy`. В репозиторий уходит только код: без данных преподавателя, работ, журналов, сборок (`.gitignore`).
* В тестах, примерах и документации — **только вымышленные ФИО** (Лесовиков, Громобоева, Пряничков…).
* Релиз: поднять версию в `src-tauri/Cargo.toml` и `tauri.conf.json` и отправить в `main` → GitHub Actions соберёт `ProverkaZadaniy.exe` и `Proverka-Setup.exe` и, если такой версии ещё нет, создаст релиз `vX.Y.Z`.

## Правила оценки (`Core::finalize`)
1. Базовая оценка — `grade` от модели (2–5); при двойной проверке — по `double_check.policy` (`primary`/`min`/`avg`), расхождение больше `max_diff` → флаг `review`.
2. `settings.caps` (шаблоны через `|` по имени файла и группы): `mode = "max"` → не выше `max`, `mode = "minus"` → минус `max` баллов, не ниже 2.
3. `settings.rules`: `min_words {enabled, words, max}`, `screenshots {enabled, max}` (если `scan.task_needs_images` и в работе нет изображений), `wrong_variant {enabled, max}`; плюс `variant_missing_cap` и `completeness_rules` (< 34 % → 2, < 60 % → максимум 3).
4. Если `penalties.enabled`: сильная улика (`ai_markers`) → 2; `ai_detected` модели с уверенностью ≥ `ai_confidence` (`ai_model`) → 2; `ai_multi` — разных средних/сильных улик ≥ `ai_multi_count` → 2; `injection` — приписка для проверяющей нейросети → 2; совпадение ≥ `plagiarism_threshold` (`plagiarism`) → 2, но с учётом `Core::plag_info`: группа одинаковых (связные пары `ws.plag`) размером ≤ `plagiarism_allowed` зачитывается; при `plagiarism_over = "first"` зачитываются первые `plagiarism_allowed` по mtime файла (Moodle `unzip` выставляет mtime из zip, `ZipEntry.mtime`). `compute_plagiarism` и смена `penalties` вызывают `refinalize_plag`. В `detect::plagiarism` почти одинаковые работы (≥ 85 %) объединяются, прежде чем считать «частые» шинглы.
5. Ручная оценка преподавателя (`override_grade`) всегда главнее.
6. Оценки — `f64` с шагом 0,5 (`settings.half_grades`, `core::norm_grade`, вывод через `core::fmt_grade` → «4,5»).
7. Доводы модели об ИИ сверяются с текстом (`Core::verify_model_evidence`): без подтверждения вывод модели не учитывается.
8. `penalties.decide = "ask"`: вместо «2» — `Outcome.ai_suspect` (оценка по содержанию, признаки с фрагментами), решение преподавателя в `Outcome.ai_decision` (`keep`/`two`), метод `api::ai_decide`, пересчёт `Core::refinalize`. `finalize` возвращает `Verdict`.

## Соглашения
* Весь пользовательский текст на русском. Комментарии в коде тоже на русском.
* UI — один файл `ui/app.js` без сборщиков. Всегда экранируйте данные через `esc()`. Новые методы ядра вызываются через `api(method, payload)`.
* Дизайн: «тетрадь в клетку + красная ручка + УФ-лампа»; темы — см. «Оформление». Класс `.empty` занят пустыми состояниями — у `.grade.empty` padding сброшен. Звуки синтезируются WebAudio в модуле `SND` (`settings.sound`, `settings.sound_volume`). Шрифты Lora (заголовки, оценки) и Inter (интерфейс) лежат локально в `ui/fonts` (OFL). Оценки рисуются SVG-штрихом (`gradeSvg`).
* Не ломайте формат `session.json` и `settings.json`: новые ключи добавляйте в `settings::defaults()`, слияние происходит через `settings::merge`.
* После изменений в `engine` прогоните `tools/devserver` (`cargo test`, `check`), затем `Собрать.bat` (или `tools\ci-build.bat`) и посмотрите `build.log`.
* Tauri 2: глобальный API включён (`withGlobalTauri`), разрешения в `src-tauri/capabilities/default.json` (`core:default`, `dialog:default`, `notification:default`). Release-сборка обычным `cargo build --release` работает благодаря фиче `custom-protocol` по умолчанию в `Cargo.toml`.

## Идеи для развития
* Распознавание скриншотов в работах (мультимодальные модели: передавать изображения из `word/media`).
* Скрытый текст в PDF (цвет заливки через pdf.js `getOperatorList`).
* Экспорт ведомости по шаблону вуза, несколько групп в отдельных листах XLSX.
* Шаблоны ведомости вуза; несколько групп в отдельных листах XLSX.
* Автор: Spezio, вопросы — Telegram @ASpezio (показывается в приложении).
