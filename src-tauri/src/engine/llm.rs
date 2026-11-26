//! Подключение к нейросетям: Ollama, llama.cpp, LM Studio, облачные
//! OpenAI-совместимые API (OpenAI, OpenRouter, DeepSeek, Groq, Mistral, Gemini,
//! YandexGPT…) и Anthropic Claude. Поддерживается вызов инструментов (tools),
//! которые приходят из MCP-сервера приложения.

use super::http::HttpClient;
use super::json::{self, Json};

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub kind: String, // ollama | llamacpp | lmstudio | openai | anthropic
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    pub max_tokens: i64,
    pub num_ctx: i64,
    pub structured: bool,
    pub timeout_s: u64,
    /// просить «размышляющие» модели (Qwen3, Nemotron, DeepSeek-R1) отвечать без рассуждений
    pub no_think: bool,
    // ---- тонкие настройки генерации (None — не передавать, берётся значение сервера)
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub min_p: Option<f64>,
    pub repeat_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<i64>,
    /// строки, на которых модель останавливается
    pub stop: Vec<String>,
    /// усилие рассуждений для облачных «думающих» моделей: low | medium | high
    pub reasoning_effort: String,
    /// дополнительная инструкция модели (добавляется к системной)
    pub system_extra: String,
    /// Ollama: сколько держать модель в памяти («10m», «1h», «-1» — всегда)
    pub keep_alive: String,
    /// Ollama: слоёв на видеокарте и потоков процессора
    pub num_gpu: Option<i64>,
    pub num_thread: Option<i64>,
    /// сколько раз повторять запрос при сетевой ошибке
    pub retries: u32,
}

impl Provider {
    pub fn from_json(j: &Json) -> Provider {
        let kind = j.get("kind").str_or("lmstudio");
        let def_url = match kind.as_str() {
            "ollama" => "http://localhost:11434",
            "llamacpp" => "http://localhost:8080/v1",
            "lmstudio" => "http://localhost:1234/v1",
            "anthropic" => "https://api.anthropic.com",
            _ => "https://api.openai.com/v1",
        };
        let local = matches!(kind.as_str(), "ollama" | "llamacpp" | "lmstudio");
        Provider {
            id: j.get("id").str_or(&kind),
            name: j.get("name").str_or(&kind),
            base_url: {
                let u = j.get("base_url").str_or(def_url);
                let u = if u.trim().is_empty() { def_url.to_string() } else { u };
                u.trim().trim_end_matches('/').to_string()
            },
            api_key: j.get("api_key").str_or(""),
            model: j.get("model").str_or(""),
            temperature: j.get("temperature").f64_or(0.1),
            max_tokens: j.get("max_tokens").i64_or(3000),
            num_ctx: j.get("num_ctx").i64_or(if local { 8192 } else { 100000 }),
            structured: j.get("structured").bool_or(local),
            timeout_s: j.get("timeout_s").i64_or(if local { 600 } else { 180 }).max(10) as u64,
            no_think: j.get("no_think").bool_or(local),
            top_p: opt_f(j, "top_p"),
            top_k: opt_f(j, "top_k").map(|v| v as i64),
            min_p: opt_f(j, "min_p"),
            repeat_penalty: opt_f(j, "repeat_penalty"),
            presence_penalty: opt_f(j, "presence_penalty"),
            frequency_penalty: opt_f(j, "frequency_penalty"),
            seed: opt_f(j, "seed").map(|v| v as i64),
            stop: match j.get("stop") {
                Json::Arr(_) => j.get("stop").str_list(),
                _ => j.get("stop").str_or("").split(['\n', ',']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
            },
            reasoning_effort: j.get("reasoning_effort").str_or(""),
            system_extra: j.get("system_extra").str_or(""),
            keep_alive: j.get("keep_alive").str_or(""),
            num_gpu: opt_f(j, "num_gpu").map(|v| v as i64),
            num_thread: opt_f(j, "num_thread").map(|v| v as i64),
            retries: j.get("retries").i64_or(1).clamp(0, 5) as u32,
            kind,
        }
    }

    /// Можно ли слать нестандартные параметры (top_k, min_p, repeat_penalty): локальные
    /// серверы и совместимые с OpenAI сторонние API их понимают, сам OpenAI — нет.
    fn extended_params(&self) -> bool {
        self.is_local() || !(self.base_url.contains("api.openai.com") || self.base_url.contains("anthropic.com"))
    }
    pub fn is_local(&self) -> bool {
        matches!(self.kind.as_str(), "ollama" | "llamacpp" | "lmstudio")
    }
    fn headers(&self) -> Vec<(String, String)> {
        let mut h = vec![("Content-Type".to_string(), "application/json".to_string())];
        match self.kind.as_str() {
            "anthropic" => {
                h.push(("x-api-key".into(), self.api_key.clone()));
                h.push(("anthropic-version".into(), "2023-06-01".into()));
            }
            _ => {
                if !self.api_key.trim().is_empty() {
                    h.push(("Authorization".into(), format!("Bearer {}", self.api_key.trim())));
                }
                if self.base_url.contains("openrouter.ai") {
                    h.push(("HTTP-Referer".into(), "https://localhost/proverka".into()));
                    h.push(("X-Title".into(), "Proverka zadaniy".into()));
                }
            }
        }
        h
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Json,
}

#[derive(Clone, Debug)]
pub enum Msg {
    System(String),
    User(String),
    /// Сообщение пользователя с картинками: (MIME, base64).
    UserImages { text: String, images: Vec<(String, String)> },
    Assistant { content: String, tool_calls: Vec<ToolCall> },
    Tool { call_id: String, name: String, content: String },
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Json,
}

#[derive(Clone, Debug, Default)]
pub struct ChatResult {
    pub content: String,
    /// ответ оборван лимитом токенов
    pub truncated: bool,
    pub tool_calls: Vec<ToolCall>,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Debug)]
pub enum LlmError {
    ToolsUnsupported(String),
    /// Запрос не помещается в контекст модели (n_ctx, prompt_tokens — если сервер их сообщил)
    Context { n_ctx: Option<i64>, prompt_tokens: Option<i64>, raw: String },
    Other(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::ToolsUnsupported(s) => write!(f, "модель не поддерживает инструменты: {}", s),
            LlmError::Context { n_ctx, .. } => write!(
                f,
                "запрос не помещается в контекст модели{}. Увеличьте длину контекста в LM Studio / Ollama или уменьшите «Максимум символов работы»",
                n_ctx.map(|n| format!(" ({} токенов)", n)).unwrap_or_default()
            ),
            LlmError::Other(s) => write!(f, "{}", s),
        }
    }
}

fn short(s: &str) -> String {
    let t: String = s.chars().take(400).collect();
    t.replace('\n', " ")
}

/// Список доступных моделей.
pub fn list_models(http: &dyn HttpClient, p: &Provider) -> Result<Vec<String>, String> {
    let url = match p.kind.as_str() {
        "ollama" => format!("{}/api/tags", p.base_url),
        "anthropic" => format!("{}/v1/models?limit=100", p.base_url),
        _ => format!("{}/models", p.base_url),
    };
    let r = http.request("GET", &url, &p.headers(), None, 15)?;
    if r.status >= 400 {
        return Err(format!("HTTP {}: {}", r.status, short(&r.body)));
    }
    let j = json::parse(&r.body).map_err(|e| format!("ответ не JSON: {}", e))?;
    let mut out: Vec<String> = Vec::new();
    for m in j.get("models").as_arr() {
        let n = m.get("name").str_or(&m.get("model").str_or(""));
        if !n.is_empty() {
            out.push(n);
        }
    }
    for m in j.get("data").as_arr() {
        let n = m.get("id").str_or("");
        if !n.is_empty() {
            out.push(n);
        }
    }
    // LM Studio отдаёт и эмбеддинг-модели — их в конец
    out.sort_by_key(|n| (n.to_lowercase().contains("embed"), n.to_lowercase()));
    out.dedup();
    Ok(out)
}

/// Схема ответа проверки (для структурированного вывода).
pub fn grade_schema() -> Json {
    let s = |t: &str| Json::obj().with("type", t);
    let arr_str = Json::obj().with("type", "array").with("items", s("string"));
    let task_item = Json::obj()
        .with("type", "object")
        .with(
            "properties",
            Json::obj()
                .with("n", s("string"))
                .with("status", Json::obj().with("type", "string").with("enum", vec!["верно".to_string(), "есть ошибки".into(), "частично".into(), "не выполнено".into()]))
                .with("comment", s("string")),
        )
        .with("required", vec!["n".to_string(), "status".into(), "comment".into()]);
    Json::obj()
        .with("type", "object")
        .with(
            "properties",
            Json::obj()
                .with("variant", s("string"))
                .with("completeness", Json::obj().with("type", "integer").with("minimum", 0).with("maximum", 100))
                .with("tasks", Json::obj().with("type", "array").with("items", task_item))
                .with("ai_detected", s("boolean"))
                .with("ai_confidence", Json::obj().with("type", "number").with("minimum", 0).with("maximum", 1))
                .with("ai_evidence", arr_str.clone())
                .with("errors", arr_str)
                .with("grade", Json::obj().with("type", "number").with("minimum", 2).with("maximum", 5))
                .with("summary", s("string"))
                // баллы по рубрике преподавателя (если она задана для задания)
                .with(
                    "rubric",
                    Json::obj().with("type", "array").with(
                        "items",
                        Json::obj().with("type", "object").with("properties", Json::obj().with("name", s("string")).with("points", s("number")).with("comment", s("string"))).with("required", vec!["name".to_string(), "points".into()]),
                    ),
                ),
        )
        .with(
            "required",
            vec![
                "variant".to_string(),
                "completeness".into(),
                "tasks".into(),
                "ai_detected".into(),
                "ai_confidence".into(),
                "ai_evidence".into(),
                "errors".into(),
                "grade".into(),
                "summary".into(),
            ],
        )
}

/// Число из настройки или None (пустое поле — «как у сервера»).
fn opt_f(j: &Json, k: &str) -> Option<f64> {
    j.get(k).as_f64().or_else(|| j.get(k).as_str().and_then(|s| s.trim().replace(',', ".").parse::<f64>().ok()))
}

pub fn chat(http: &dyn HttpClient, p: &Provider, msgs: &[Msg], tools: &[ToolDef], schema: Option<&Json>) -> Result<ChatResult, LlmError> {
    if p.model.trim().is_empty() {
        return Err(LlmError::Other("не выбрана модель".into()));
    }
    // своя инструкция для этой модели — к системной
    let with_extra: Vec<Msg>;
    let msgs = if !p.system_extra.trim().is_empty() {
        with_extra = msgs
            .iter()
            .enumerate()
            .map(|(i, m)| match m {
                Msg::System(s) if i == 0 => Msg::System(format!("{}\n\n{}", s, p.system_extra.trim())),
                other => other.clone(),
            })
            .collect();
        &with_extra[..]
    } else {
        msgs
    };
    // повтор при сетевой ошибке (не при ответе сервера об ошибке)
    let mut attempt = 0;
    loop {
        match chat_once(http, p, msgs, tools, schema) {
            Err(LlmError::Other(e)) if attempt < p.retries && (e.contains("соедин") || e.contains("onnect") || e.contains("timed out") || e.contains("таймаут") || e.contains("reset")) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(1500 * attempt as u64));
            }
            r => return r,
        }
    }
}

fn chat_once(http: &dyn HttpClient, p: &Provider, msgs: &[Msg], tools: &[ToolDef], schema: Option<&Json>) -> Result<ChatResult, LlmError> {
    // «/no_think» в системной инструкции отключает длинные рассуждения у Qwen3, Nemotron и др.
    let patched: Vec<Msg>;
    let msgs = if p.no_think {
        patched = msgs
            .iter()
            .enumerate()
            .map(|(i, m)| match m {
                Msg::System(s) if i == 0 && !s.contains("/no_think") => Msg::System(format!("{}\n/no_think", s)),
                other => other.clone(),
            })
            .collect();
        &patched[..]
    } else {
        msgs
    };
    match p.kind.as_str() {
        "ollama" => chat_ollama(http, p, msgs, tools, schema),
        "anthropic" => chat_anthropic(http, p, msgs, tools),
        _ => chat_openai(http, p, msgs, tools, schema),
    }
}

fn send(http: &dyn HttpClient, p: &Provider, url: &str, body: &Json, with_tools: bool) -> Result<Json, LlmError> {
    let b = body.to_string();
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1500 * attempt));
        }
        match http.request("POST", url, &p.headers(), Some(&b), p.timeout_s) {
            Ok(r) => {
                if r.status >= 400 {
                    if let Some(e) = context_error(&r.body) {
                        return Err(e);
                    }
                }
                if r.status == 429 || r.status >= 500 {
                    last_err = format!("HTTP {}: {}", r.status, short(&r.body));
                    continue;
                }
                if r.status >= 400 {
                    let low = r.body.to_lowercase();
                    if with_tools && (low.contains("tool") || low.contains("function")) {
                        return Err(LlmError::ToolsUnsupported(short(&r.body)));
                    }
                    return Err(LlmError::Other(format!("HTTP {}: {}", r.status, short(&r.body))));
                }
                return json::parse(&r.body).map_err(|e| LlmError::Other(format!("ответ модели не JSON: {} — {}", e, short(&r.body))));
            }
            Err(e) => {
                last_err = e;
                // таймаут чтения повторять бессмысленно — модель просто долго думает
                if last_err.contains("timed out") || last_err.contains("тайм") {
                    break;
                }
            }
        }
    }
    Err(LlmError::Other(last_err))
}

/// Число сразу после шаблона (пропуская кавычки, двоеточия, пробелы, скобки).
fn num_after(s: &str, pat: &str) -> Option<i64> {
    let i = s.find(pat)? + pat.len();
    let rest: String = s[i..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    // число должно идти почти сразу за шаблоном
    let gap = s[i..].chars().take_while(|c| !c.is_ascii_digit()).count();
    if gap > 6 {
        return None;
    }
    rest.parse().ok()
}

/// Распознаёт ошибку «запрос длиннее контекста» у LM Studio, llama.cpp, Ollama, OpenAI и др.
pub fn context_error(body: &str) -> Option<LlmError> {
    let l = body.to_lowercase();
    let is_ctx = (l.contains("context") && (l.contains("exceed") || l.contains("too long") || l.contains("maximum context") || l.contains("context size") || l.contains("context length") || l.contains("n_ctx") || l.contains("overflow") || l.contains("greater than")))
        || l.contains("exceed_context_size")
        || l.contains("context_length_exceeded")
        || l.contains("prompt is too long")
        || l.contains("tokens to keep")
        || l.contains("input is too long")
        || l.contains("too many tokens")
        || l.contains("maximum number of tokens")
        || (l.contains("token") && l.contains("limit") && (l.contains("prompt") || l.contains("input")));
    if !is_ctx {
        return None;
    }
    let n_ctx = num_after(&l, "\"n_ctx\\\":")
        .or_else(|| num_after(&l, "\"n_ctx\":"))
        .or_else(|| num_after(&l, "n_ctx"))
        .or_else(|| num_after(&l, "available context size ("))
        .or_else(|| num_after(&l, "context size ("))
        .or_else(|| num_after(&l, "maximum context length is "))
        .or_else(|| num_after(&l, "context length of "))
        .or_else(|| num_after(&l, "context window of "))
        .or_else(|| num_after(&l, "context length ("))
        .or_else(|| num_after(&l, "context length is "))
        .or_else(|| num_after(&l, "the context length "));
    let prompt_tokens = num_after(&l, "n_prompt_tokens")
        .or_else(|| num_after(&l, "request ("))
        .or_else(|| num_after(&l, "resulted in "))
        .or_else(|| num_after(&l, "prompt is too long: "))
        .or_else(|| num_after(&l, "prompt has "))
        .or_else(|| num_after(&l, "the first "));
    Some(LlmError::Context { n_ctx, prompt_tokens, raw: short(body) })
}

// ------------------------------------------------------------------ OpenAI-совместимые

fn chat_openai(http: &dyn HttpClient, p: &Provider, msgs: &[Msg], tools: &[ToolDef], schema: Option<&Json>) -> Result<ChatResult, LlmError> {
    let mut m = Json::arr();
    for x in msgs {
        match x {
            Msg::System(s) => m.push(Json::obj().with("role", "system").with("content", s.as_str())),
            Msg::User(s) => m.push(Json::obj().with("role", "user").with("content", s.as_str())),
            Msg::UserImages { text, images } => {
                let mut parts = Json::arr();
                parts.push(Json::obj().with("type", "text").with("text", text.as_str()));
                for (mime, b64) in images {
                    parts.push(Json::obj().with("type", "image_url").with("image_url", Json::obj().with("url", format!("data:{};base64,{}", mime, b64))));
                }
                m.push(Json::obj().with("role", "user").with("content", parts));
            }
            Msg::Assistant { content, tool_calls } => {
                let mut o = Json::obj().with("role", "assistant").with("content", content.as_str());
                if !tool_calls.is_empty() {
                    let mut tc = Json::arr();
                    for c in tool_calls {
                        tc.push(
                            Json::obj()
                                .with("id", c.id.as_str())
                                .with("type", "function")
                                .with("function", Json::obj().with("name", c.name.as_str()).with("arguments", c.args.to_string())),
                        );
                    }
                    o.set("tool_calls", tc);
                }
                m.push(o);
            }
            Msg::Tool { call_id, name, content } => m.push(
                Json::obj()
                    .with("role", "tool")
                    .with("tool_call_id", call_id.as_str())
                    .with("name", name.as_str())
                    .with("content", content.as_str()),
            ),
        }
    }
    let mut body = Json::obj()
        .with("model", p.model.as_str())
        .with("messages", m)
        .with("temperature", p.temperature)
        .with("max_tokens", p.max_tokens)
        .with("stream", false);
    if let Some(v) = p.top_p {
        body.set("top_p", v);
    }
    if let Some(v) = p.presence_penalty {
        body.set("presence_penalty", v);
    }
    if let Some(v) = p.frequency_penalty {
        body.set("frequency_penalty", v);
    }
    if let Some(v) = p.seed {
        body.set("seed", v);
    }
    if !p.stop.is_empty() {
        body.set("stop", p.stop.clone());
    }
    if !p.reasoning_effort.is_empty() {
        body.set("reasoning_effort", p.reasoning_effort.as_str());
    }
    if p.extended_params() {
        if let Some(v) = p.top_k {
            body.set("top_k", v);
        }
        if let Some(v) = p.min_p {
            body.set("min_p", v);
        }
        if let Some(v) = p.repeat_penalty {
            body.set("repeat_penalty", v);
        }
    }
    if !tools.is_empty() {
        let mut t = Json::arr();
        for d in tools {
            t.push(
                Json::obj().with("type", "function").with(
                    "function",
                    Json::obj().with("name", d.name.as_str()).with("description", d.description.as_str()).with("parameters", d.schema.clone()),
                ),
            );
        }
        body.set("tools", t);
        body.set("tool_choice", "auto");
    } else if let (Some(s), true) = (schema, p.structured) {
        body.set(
            "response_format",
            Json::obj()
                .with("type", "json_schema")
                .with("json_schema", Json::obj().with("name", "grade_result").with("schema", s.clone())),
        );
    }
    let url = format!("{}/chat/completions", p.base_url);
    let r = match send(http, p, &url, &body, !tools.is_empty()) {
        // некоторые серверы не знают response_format — повторяем без него
        Err(LlmError::Other(e)) if body.get("response_format").as_obj().len() > 0 && (e.contains("response_format") || e.contains("json_schema")) => {
            if let Json::Obj(items) = &mut body {
                items.retain(|(k, _)| k != "response_format");
            }
            send(http, p, &url, &body, false)?
        }
        other => other?,
    };
    let msg = r.get("choices").idx(0).get("message");
    let mut res = ChatResult {
        content: msg.get("content").str_or(""),
        truncated: r.get("choices").idx(0).get("finish_reason").str_or("") == "length",
        tokens_in: r.get("usage").get("prompt_tokens").i64_or(0),
        tokens_out: r.get("usage").get("completion_tokens").i64_or(0),
        ..Default::default()
    };
    if res.content.is_empty() {
        // некоторые «размышляющие» модели кладут ответ в reasoning_content
        res.content = msg.get("reasoning_content").str_or("");
    }
    for (i, c) in msg.get("tool_calls").as_arr().iter().enumerate() {
        let f = c.get("function");
        let args_raw = f.get("arguments");
        let args = match args_raw {
            Json::Str(s) => json::parse(s).ok().or_else(|| json::extract_object(s)).unwrap_or(Json::obj()),
            Json::Obj(_) => args_raw.clone(),
            _ => Json::obj(),
        };
        res.tool_calls.push(ToolCall { id: c.get("id").str_or(&format!("call_{}", i)), name: f.get("name").str_or(""), args });
    }
    Ok(res)
}

// ------------------------------------------------------------------ Ollama

fn chat_ollama(http: &dyn HttpClient, p: &Provider, msgs: &[Msg], tools: &[ToolDef], schema: Option<&Json>) -> Result<ChatResult, LlmError> {
    let mut m = Json::arr();
    for x in msgs {
        match x {
            Msg::System(s) => m.push(Json::obj().with("role", "system").with("content", s.as_str())),
            Msg::User(s) => m.push(Json::obj().with("role", "user").with("content", s.as_str())),
            Msg::UserImages { text, images } => m.push(
                Json::obj()
                    .with("role", "user")
                    .with("content", text.as_str())
                    .with("images", Json::Arr(images.iter().map(|(_, b)| Json::from(b.as_str())).collect())),
            ),
            Msg::Assistant { content, tool_calls } => {
                let mut o = Json::obj().with("role", "assistant").with("content", content.as_str());
                if !tool_calls.is_empty() {
                    let mut tc = Json::arr();
                    for c in tool_calls {
                        tc.push(Json::obj().with("function", Json::obj().with("name", c.name.as_str()).with("arguments", c.args.clone())));
                    }
                    o.set("tool_calls", tc);
                }
                m.push(o);
            }
            Msg::Tool { name, content, .. } => m.push(Json::obj().with("role", "tool").with("tool_name", name.as_str()).with("content", content.as_str())),
        }
    }
    let mut body = Json::obj()
        .with("model", p.model.as_str())
        .with("messages", m)
        .with("stream", false)
        .with("think", !p.no_think)
        .with("options", {
            let mut o = Json::obj().with("temperature", p.temperature).with("num_ctx", p.num_ctx).with("num_predict", p.max_tokens);
            for (k, v) in [("top_p", p.top_p), ("min_p", p.min_p), ("repeat_penalty", p.repeat_penalty), ("presence_penalty", p.presence_penalty), ("frequency_penalty", p.frequency_penalty)] {
                if let Some(v) = v {
                    o.set(k, v);
                }
            }
            for (k, v) in [("top_k", p.top_k), ("seed", p.seed), ("num_gpu", p.num_gpu), ("num_thread", p.num_thread)] {
                if let Some(v) = v {
                    o.set(k, v);
                }
            }
            if !p.stop.is_empty() {
                o.set("stop", p.stop.clone());
            }
            o
        });
    if !p.keep_alive.trim().is_empty() {
        let ka = p.keep_alive.trim();
        match ka.parse::<i64>() {
            Ok(n) => body.set("keep_alive", n),
            Err(_) => body.set("keep_alive", ka),
        }
    }
    if !tools.is_empty() {
        let mut t = Json::arr();
        for d in tools {
            t.push(
                Json::obj().with("type", "function").with(
                    "function",
                    Json::obj().with("name", d.name.as_str()).with("description", d.description.as_str()).with("parameters", d.schema.clone()),
                ),
            );
        }
        body.set("tools", t);
    } else if let (Some(s), true) = (schema, p.structured) {
        body.set("format", s.clone());
    }
    let r = send(http, p, &format!("{}/api/chat", p.base_url), &body, !tools.is_empty())?;
    let msg = r.get("message");
    let mut res = ChatResult {
        content: msg.get("content").str_or(""),
        truncated: r.get("done_reason").str_or("") == "length",
        tokens_in: r.get("prompt_eval_count").i64_or(0),
        tokens_out: r.get("eval_count").i64_or(0),
        ..Default::default()
    };
    for (i, c) in msg.get("tool_calls").as_arr().iter().enumerate() {
        let f = c.get("function");
        let args = match f.get("arguments") {
            Json::Str(s) => json::parse(s).unwrap_or(Json::obj()),
            o => o.clone(),
        };
        res.tool_calls.push(ToolCall { id: format!("call_{}", i), name: f.get("name").str_or(""), args });
    }
    Ok(res)
}

// ------------------------------------------------------------------ Anthropic

fn chat_anthropic(http: &dyn HttpClient, p: &Provider, msgs: &[Msg], tools: &[ToolDef]) -> Result<ChatResult, LlmError> {
    let mut system = String::new();
    let mut out: Vec<(String, Json)> = Vec::new(); // (role, content-blocks array)
    let push_block = |role: &str, block: Json, out: &mut Vec<(String, Json)>| {
        if let Some(last) = out.last_mut() {
            if last.0 == role {
                last.1.push(block);
                return;
            }
        }
        let mut a = Json::arr();
        a.push(block);
        out.push((role.to_string(), a));
    };
    for x in msgs {
        match x {
            Msg::System(s) => {
                system.push_str(s);
                system.push('\n');
            }
            Msg::User(s) => push_block("user", Json::obj().with("type", "text").with("text", s.as_str()), &mut out),
            Msg::UserImages { text, images } => {
                for (mime, b64) in images {
                    push_block(
                        "user",
                        Json::obj().with("type", "image").with("source", Json::obj().with("type", "base64").with("media_type", mime.as_str()).with("data", b64.as_str())),
                        &mut out,
                    );
                }
                push_block("user", Json::obj().with("type", "text").with("text", text.as_str()), &mut out);
            }
            Msg::Assistant { content, tool_calls } => {
                if !content.trim().is_empty() {
                    push_block("assistant", Json::obj().with("type", "text").with("text", content.as_str()), &mut out);
                }
                for c in tool_calls {
                    push_block(
                        "assistant",
                        Json::obj().with("type", "tool_use").with("id", c.id.as_str()).with("name", c.name.as_str()).with("input", c.args.clone()),
                        &mut out,
                    );
                }
            }
            Msg::Tool { call_id, content, .. } => push_block(
                "user",
                Json::obj().with("type", "tool_result").with("tool_use_id", call_id.as_str()).with("content", content.as_str()),
                &mut out,
            ),
        }
    }
    let mut messages = Json::arr();
    for (role, blocks) in out {
        messages.push(Json::obj().with("role", role).with("content", blocks));
    }
    let mut body = Json::obj()
        .with("model", p.model.as_str())
        .with("max_tokens", p.max_tokens.max(1024))
        .with("temperature", p.temperature)
        .with("system", system.trim())
        .with("messages", messages);
    if let Some(v) = p.top_p {
        body.set("top_p", v);
    }
    if let Some(v) = p.top_k {
        body.set("top_k", v);
    }
    if !p.stop.is_empty() {
        body.set("stop_sequences", p.stop.clone());
    }
    if !tools.is_empty() {
        let mut t = Json::arr();
        for d in tools {
            t.push(Json::obj().with("name", d.name.as_str()).with("description", d.description.as_str()).with("input_schema", d.schema.clone()));
        }
        body.set("tools", t);
    }
    let r = send(http, p, &format!("{}/v1/messages", p.base_url), &body, !tools.is_empty())?;
    let mut res = ChatResult {
        truncated: r.get("stop_reason").str_or("") == "max_tokens",
        tokens_in: r.get("usage").get("input_tokens").i64_or(0),
        tokens_out: r.get("usage").get("output_tokens").i64_or(0),
        ..Default::default()
    };
    for b in r.get("content").as_arr() {
        match b.get("type").as_str() {
            Some("text") => res.content.push_str(&b.get("text").str_or("")),
            Some("tool_use") => res.tool_calls.push(ToolCall { id: b.get("id").str_or(""), name: b.get("name").str_or(""), args: b.get("input").clone() }),
            _ => {}
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctx_errors() {
        let lm = r#"{"error":"Engine protocol predict request returned 400: {\"error\":{\"code\":400,\"message\":\"request (9143 tokens) exceeds the available context size (8192 tokens), try increasing it\",\"type\":\"exceed_context_size_error\",\"n_prompt_tokens\":9143,\"n_ctx\":8192}}"}"#;
        match context_error(lm) {
            Some(LlmError::Context { n_ctx, prompt_tokens, .. }) => {
                assert_eq!(n_ctx, Some(8192));
                assert_eq!(prompt_tokens, Some(9143));
            }
            _ => panic!("не распознано"),
        }
        let oa = r#"{"error":{"message":"This model's maximum context length is 16385 tokens. However, your messages resulted in 20000 tokens.","code":"context_length_exceeded"}}"#;
        match context_error(oa) {
            Some(LlmError::Context { n_ctx, prompt_tokens, .. }) => {
                assert_eq!(n_ctx, Some(16385));
                assert_eq!(prompt_tokens, Some(20000));
            }
            _ => panic!("не распознано"),
        }
        assert!(context_error(r#"{"error":"model not found"}"#).is_none());
    }
}
