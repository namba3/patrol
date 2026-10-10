#![allow(non_snake_case)]

use dioxus::prelude::*;
use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use futures_util::StreamExt;
use gloo_net::{
    http::Request,
    websocket::{futures::WebSocket, Message},
};
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use unic_langid::LanguageIdentifier;

const STATUS_VIEW_STORAGE_KEY: &str = "patrol.status-view.v1";
const LANGUAGE_STORAGE_KEY: &str = "patrol.language.v1";
const HISTORY_PAGE_SIZE: usize = 50;
const EVENT_LIMIT: usize = 20;
const MAIN_CSS: Asset = asset!("/assets/main.css");

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct DocStatus {
    id: String,
    url: String,
    status: String,
    last_updated_unix_ms: Option<i64>,
    last_checked_unix_ms: Option<i64>,
    last_attempted_unix_ms: Option<i64>,
    last_success_unix_ms: Option<i64>,
    consecutive_failures: u32,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct DocUpdateInfo {
    event: String,
    id: String,
    url: String,
    timestamp: String,
    consecutive_failures: Option<u32>,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct ChangeHistoryEntry {
    id: String,
    timestamp_unix_ms: i64,
    previous_content: Option<String>,
    previous_truncated: bool,
    content: String,
    content_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct HistoryPage {
    entries: Vec<ChangeHistoryEntry>,
    offset: usize,
    limit: usize,
    total: usize,
    has_older: bool,
    has_newer: bool,
}

#[derive(Clone, Debug)]
struct DiffLine {
    prefix: &'static str,
    text: String,
    class_name: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
enum ApiError {
    Timeout,
    Http(u16),
    Request,
    InvalidResponse,
}

type UiBundle = FluentBundle<FluentResource>;

thread_local! {
    static JA_BUNDLE: RefCell<UiBundle> = RefCell::new(make_bundle(
        "ja-JP",
        include_str!("../locales/ja.ftl"),
    ));
    static EN_BUNDLE: RefCell<UiBundle> = RefCell::new(make_bundle(
        "en-US",
        include_str!("../locales/en.ftl"),
    ));
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StatusView {
    search: String,
    status: String,
    sort: String,
    refresh_ms: u32,
}

impl Default for StatusView {
    fn default() -> Self {
        Self {
            search: String::new(),
            status: String::new(),
            sort: "last_updated".into(),
            refresh_ms: 15_000,
        }
    }
}

fn main() {
    dioxus::launch(app);
}

fn make_bundle(locale: &str, source: &str) -> UiBundle {
    let locale: LanguageIdentifier = locale.parse().expect("UI locale is valid");
    let mut bundle = FluentBundle::new(vec![locale]);
    let resource = FluentResource::try_new(source.to_owned())
        .unwrap_or_else(|(_, errors)| panic!("UI Fluent catalog is invalid: {errors:?}"));
    bundle
        .add_resource(resource)
        .expect("UI Fluent catalog has no duplicate message IDs");
    bundle
}

fn missing_message(key: &str) -> String {
    #[cfg(debug_assertions)]
    {
        format!("[MISSING: {key}]")
    }
    #[cfg(not(debug_assertions))]
    {
        key.to_owned()
    }
}

fn tr(locale: &str, key: &str) -> String {
    tr_args(locale, key, &[])
}

fn tr_args(locale: &str, key: &str, args: &[(&str, String)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, value.as_str());
    }
    format_message(locale, key, Some(&fluent_args)).unwrap_or_else(|| missing_message(key))
}

fn tr_count(locale: &str, key: &str, count: usize, args: &[(&str, String)]) -> String {
    let mut fluent_args = FluentArgs::new();
    fluent_args.set("count", count as i64);
    fluent_args.set("count-display", format_number(count, locale));
    for (name, value) in args {
        fluent_args.set(*name, value.as_str());
    }
    format_message(locale, key, Some(&fluent_args)).unwrap_or_else(|| missing_message(key))
}

fn format_message(locale: &str, key: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
    let selected = if locale == "ja" {
        JA_BUNDLE.with(|bundle| format_from_bundle(&bundle.borrow(), key, args))
    } else {
        EN_BUNDLE.with(|bundle| format_from_bundle(&bundle.borrow(), key, args))
    };
    selected.or_else(|| EN_BUNDLE.with(|bundle| format_from_bundle(&bundle.borrow(), key, args)))
}

fn format_from_bundle(
    bundle: &UiBundle,
    key: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    Some(
        bundle
            .format_pattern(pattern, args, &mut errors)
            .into_owned(),
    )
}

fn api_error_message(locale: &str, error: &ApiError) -> String {
    match error {
        ApiError::Timeout => tr(locale, "api-timeout"),
        ApiError::Http(status) => tr_args(
            locale,
            "api-http_error",
            &[("status", format_number(*status as usize, locale))],
        ),
        ApiError::Request => tr(locale, "api-request_error"),
        ApiError::InvalidResponse => tr(locale, "api-invalid_response"),
    }
}

fn load_locale() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(saved) = web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(LANGUAGE_STORAGE_KEY).ok().flatten())
        {
            if saved == "ja" || saved == "en" {
                return saved;
            }
        }
        if web_sys::window()
            .and_then(|window| window.navigator().language())
            .is_some_and(|language| language.to_lowercase().starts_with("ja"))
        {
            return "ja".into();
        }
    }
    "en".into()
}

fn save_locale(locale: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(LANGUAGE_STORAGE_KEY, locale);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = locale;
}

fn set_document_language(locale: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    {
        let _ = element.set_attribute("lang", locale);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = locale;
}

fn format_number(value: usize, locale: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let browser_locale = if locale == "ja" { "ja-JP" } else { "en-US" };
        return js_sys::Number::from(value as f64)
            .to_locale_string(browser_locale)
            .into();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = locale;
        value.to_string()
    }
}

fn app() -> Element {
    let mut statuses = use_signal(Vec::<DocStatus>::new);
    let mut status_loading = use_signal(|| true);
    let mut status_error = use_signal(|| None::<ApiError>);
    let mut last_updated = use_signal(|| None::<i64>);
    let mut socket_connected = use_signal(|| false);
    let mut events = use_signal(Vec::<DocUpdateInfo>::new);
    let mut locale = use_signal(load_locale);
    let mut status_view = use_signal(load_status_view);
    let mut event_id = use_signal(String::new);
    let mut event_type = use_signal(String::new);
    let mut history_id = use_signal(|| None::<String>);
    let mut history_page = use_signal(|| None::<HistoryPage>);
    let mut history_error = use_signal(|| None::<ApiError>);
    let mut history_loading = use_signal(|| false);

    use_effect(move || {
        let selected = locale();
        save_locale(&selected);
        set_document_language(&selected);
    });

    use_future(move || {
        let interval_ms = status_view().refresh_ms;
        async move {
            loop {
                match fetch_json::<Vec<DocStatus>>("/api/v1/status").await {
                    Ok(result) => {
                        statuses.set(result);
                        last_updated.set(Some(now_unix_ms()));
                        status_error.set(None);
                        status_loading.set(false);
                    }
                    Err(error) => {
                        status_error.set(Some(error));
                        status_loading.set(false);
                    }
                }
                TimeoutFuture::new(interval_ms).await;
            }
        }
    });

    use_future(move || async move {
        let mut delay_ms = 1_000u32;
        loop {
            let scheme = if web_sys::window()
                .and_then(|window| window.location().protocol().ok())
                .is_some_and(|protocol| protocol == "https:")
            {
                "wss"
            } else {
                "ws"
            };
            let host = web_sys::window()
                .and_then(|window| window.location().host().ok())
                .unwrap_or_default();
            let address = format!("{scheme}://{host}/");

            if let Ok(socket) = WebSocket::open(&address) {
                socket_connected.set(true);
                delay_ms = 1_000;
                let mut socket = socket;
                while let Some(message) = socket.next().await {
                    match message {
                        Ok(Message::Text(message)) => {
                            if let Ok(event) = serde_json::from_str::<DocUpdateInfo>(&message) {
                                events.with_mut(|items| {
                                    items.insert(0, event);
                                    items.truncate(EVENT_LIMIT);
                                });
                            }
                        }
                        Ok(Message::Bytes(_)) => (),
                        Err(_) => break,
                    }
                }
            }
            socket_connected.set(false);
            TimeoutFuture::new(delay_ms).await;
            delay_ms = (delay_ms * 2).min(30_000);
        }
    });

    use_effect(move || {
        let view = status_view();
        if let Ok(value) = serde_json::to_string(&view) {
            save_status_view(&value);
        }
    });

    let all_statuses = statuses();
    let view = status_view();
    let visible = filtered_statuses(&all_statuses, &view);
    let ok_count = all_statuses
        .iter()
        .filter(|item| item.status == "ok")
        .count();
    let failed_count = all_statuses
        .iter()
        .filter(|item| item.status == "failed")
        .count();
    let unchecked_count = all_statuses
        .iter()
        .filter(|item| item.status == "not_checked")
        .count();
    let current_events = events();
    let visible_events = current_events
        .iter()
        .filter(|item| event_id().is_empty() || item.id == event_id())
        .filter(|item| event_type().is_empty() || item.event == event_type())
        .cloned()
        .collect::<Vec<_>>();
    let status_csv_content = status_csv(&visible);
    let events_csv_content = event_csv(&visible_events);
    let visible_status_count = visible.len();
    let visible_event_count = visible_events.len();
    let current_locale = locale();
    let status_error_text = status_error().map(|error| api_error_message(&current_locale, &error));
    let status_connection_class = if status_error_text.is_some() {
        "connection error"
    } else if socket_connected() {
        "connection connected"
    } else {
        "connection"
    };

    rsx! {
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        main { class: "app-shell", lang: "{current_locale}",
            header { class: "topbar",
                div { class: "brand",
                    div { class: "brand-mark", "P" }
                    div {
                        h1 { "Patrol" }
                        p { "{tr(&current_locale, \"app-subtitle\")}" }
                    }
                }
                div { class: "top-actions",
                    span { class: status_connection_class, role: "status", aria_live: "polite",
                        if let Some(error) = status_error_text.clone() {
                            "{tr_args(&current_locale, \"connection-api_failed\", &[(\"error\", error.clone())])}"
                        } else if socket_connected() {
                            "{tr(&current_locale, \"connection-realtime_connected\")}"
                        } else {
                            "{tr(&current_locale, \"connection-reconnecting\")}"
                        }
                    }
                    label { class: "language-picker",
                        span { class: "sr-only", "{tr(&current_locale, \"language-label\")}" }
                        select {
                            aria_label: "{tr(&current_locale, \"language-label\")}",
                            value: "{current_locale}",
                            onchange: move |event| locale.set(event.value()),
                            option { value: "ja", "{tr(&current_locale, \"language-japanese\")}" }
                            option { value: "en", "{tr(&current_locale, \"language-english\")}" }
                        }
                    }
                    button {
                        class: "primary",
                        onclick: move |_| { spawn(async move {
                            match fetch_json::<Vec<DocStatus>>("/api/v1/status").await {
                                Ok(result) => {
                                    statuses.set(result);
                                    status_error.set(None);
                                    last_updated.set(Some(now_unix_ms()));
                                    status_loading.set(false);
                                }
                                Err(error) => {
                                    status_error.set(Some(error));
                                    status_loading.set(false);
                                }
                            }
                        }); },
                        "{tr(&current_locale, \"action-refresh_now\")}"
                    }
                }
            }

            section { class: "stats", aria_label: "{tr(&current_locale, \"summary-title\")}",
                div { class: "stat",
                    div { class: "stat-label", "{tr(&current_locale, \"summary-registered_targets\")}" }
                    div { class: "stat-value", "{format_number(all_statuses.len(), &current_locale)}" }
                }
                div { class: "stat",
                    div { class: "stat-label", "{tr(&current_locale, \"summary-healthy\")}" }
                    div { class: "stat-value good", "{format_number(ok_count, &current_locale)}" }
                }
                div { class: "stat",
                    div { class: "stat-label", "{tr(&current_locale, \"summary-needs_attention\")}" }
                    div { class: "stat-value bad", "{format_number(failed_count + unchecked_count, &current_locale)}" }
                }
            }

            div { class: "page-grid", style: "margin-top: 20px",
                section { class: "main-column",
                    article { class: "card",
                        header { class: "card-head",
                            div {
                                h2 { "{tr(&current_locale, \"targets-title\")}" }
                                p {
                                    if let Some(timestamp) = last_updated() {
                                        "{tr_args(&current_locale, \"targets-last_updated\", &[(\"time\", format_time(Some(timestamp), &current_locale))])}"
                                    } else if let Some(error) = status_error_text.clone() {
                                        "{tr_args(&current_locale, \"targets-fetch_error\", &[(\"error\", error.clone())])}"
                                    } else {
                                        "{tr(&current_locale, \"targets-loading\")}"
                                    }
                                }
                            }
                            button {
                                class: "quiet",
                                disabled: visible_status_count == 0,
                                onclick: move |_| download_csv("patrol-status.csv", &status_csv_content),
                                "{tr(&current_locale, \"targets-export_csv\")}"
                            }
                        }
                        div { class: "card-body",
                            div { class: "toolbar",
                                input {
                                    class: "search",
                                    r#type: "search",
                                    placeholder: "{tr(&current_locale, \"targets-search_placeholder\")}",
                                    value: "{view.search}",
                                    oninput: move |event| status_view.with_mut(|view| view.search = event.value()),
                                }
                                select {
                                    aria_label: "{tr(&current_locale, \"filter-status_label\")}",
                                    value: "{view.status}",
                                    onchange: move |event| status_view.with_mut(|view| view.status = event.value()),
                                    option { value: "", "{tr(&current_locale, \"filter-status-all\")}" }
                                    option { value: "ok", "{tr(&current_locale, \"filter-status-ok\")}" }
                                    option { value: "failed", "{tr(&current_locale, \"filter-status-failed\")}" }
                                    option { value: "not_checked", "{tr(&current_locale, \"filter-status-not_checked\")}" }
                                }
                                select {
                                    aria_label: "{tr(&current_locale, \"filter-sort_label\")}",
                                    value: "{view.sort}",
                                    onchange: move |event| status_view.with_mut(|view| view.sort = event.value()),
                                    option { value: "id", "{tr(&current_locale, \"filter-sort-id\")}" }
                                    option { value: "status", "{tr(&current_locale, \"filter-sort-status\")}" }
                                    option { value: "last_attempted", "{tr(&current_locale, \"filter-sort-last_attempted\")}" }
                                    option { value: "last_updated", "{tr(&current_locale, \"filter-sort-last_updated\")}" }
                                }
                                select {
                                    aria_label: "{tr(&current_locale, \"filter-refresh_label\")}",
                                    value: "{view.refresh_ms}",
                                    onchange: move |event| {
                                        if let Ok(value) = event.value().parse::<u32>() {
                                            status_view.with_mut(|view| view.refresh_ms = value);
                                        }
                                    },
                                    option { value: "5000", "{tr_count(&current_locale, \"filter-refresh-seconds\", 5, &[])}" }
                                    option { value: "15000", "{tr_count(&current_locale, \"filter-refresh-seconds\", 15, &[])}" }
                                    option { value: "30000", "{tr_count(&current_locale, \"filter-refresh-seconds\", 30, &[])}" }
                                    option { value: "60000", "{tr_count(&current_locale, \"filter-refresh-seconds\", 60, &[])}" }
                                }
                                button {
                                    class: "quiet",
                                    onclick: move |_| {
                                        status_view.with_mut(|view| {
                                            let refresh_ms = view.refresh_ms;
                                            *view = StatusView::default();
                                            view.refresh_ms = refresh_ms;
                                        });
                                    },
                                    "{tr(&current_locale, \"filter-reset\")}"
                                }
                                span { class: "count", "{tr_count(&current_locale, \"targets-visible_count\", visible_status_count, &[(\"total\", format_number(all_statuses.len(), &current_locale))])}" }
                            }
                            div { class: "table-wrap", role: "region", aria_label: "{tr(&current_locale, \"targets-table_label\")}", tabindex: "0",
                                table {
                                    thead { tr {
                                        th { scope: "col", "{tr(&current_locale, \"table-target\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-status\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-last_attempted\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-last_success\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-last_change\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-error\")}" }
                                        th { scope: "col", "{tr(&current_locale, \"table-history\")}" }
                                    } }
                                    tbody {
                                        if visible.is_empty() {
                                            tr { td { colspan: "7", class: "empty",
                                                if status_loading() { "{tr(&current_locale, \"empty-status_loading\")}" }
                                                else if all_statuses.is_empty() { "{tr(&current_locale, \"empty-no_targets\")}" }
                                                else { "{tr(&current_locale, \"empty-no_filter_match\")}" }
                                            } }
                                        }
                                        for status in visible {
                                            StatusRow {
                                                key: "{status.id}",
                                                status: status,
                                                locale: current_locale.clone(),
                                                on_history: move |id: String| {
                                                    history_id.set(Some(id.clone()));
                                                    history_page.set(None);
                                                    history_error.set(None);
                                                    history_loading.set(true);
                                                    spawn(async move { load_history(id, 0, history_page, history_error, history_loading).await; });
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                aside { class: "side-column",
                    article { class: "card",
                        header { class: "card-head",
                            div {
                                h2 { "{tr(&current_locale, \"notification-title\")}" }
                                p { "{tr_count(&current_locale, \"notification-latest_count\", EVENT_LIMIT, &[])}" }
                            }
                            button {
                                class: "quiet",
                                disabled: current_events.is_empty(),
                                onclick: move |_| events.set(Vec::new()),
                                "{tr(&current_locale, \"notification-clear\")}"
                            }
                        }
                        div { class: "card-body",
                            div { class: "side-filters", style: "margin-bottom: 14px",
                                label { "{tr(&current_locale, \"notification-filter-target\")}"
                                    select {
                                        value: "{event_id()}",
                                        onchange: move |event| event_id.set(event.value()),
                                        option { value: "", "{tr(&current_locale, \"notification-filter-all_targets\")}" }
                                        for id in all_statuses.iter().map(|status| status.id.clone()) {
                                            option { key: "{id}", value: "{id}", "{id}" }
                                        }
                                    }
                                }
                                label { "{tr(&current_locale, \"notification-filter-event\")}"
                                    select {
                                        value: "{event_type()}",
                                        onchange: move |event| event_type.set(event.value()),
                                        option { value: "", "{tr(&current_locale, \"notification-filter-all_events\")}" }
                                        option { value: "changed", "{tr(&current_locale, \"notification-event-changed\")}" }
                                        option { value: "poll_failed", "{tr(&current_locale, \"notification-event-poll_failed\")}" }
                                        option { value: "poll_recovered", "{tr(&current_locale, \"notification-event-poll_recovered\")}" }
                                    }
                                }
                                button {
                                    disabled: visible_event_count == 0,
                                    onclick: move |_| download_csv("patrol-events.csv", &events_csv_content),
                                    "{tr(&current_locale, \"notification-export_csv\")}"
                                }
                            }
                            div { class: "event-list", id: "events", aria_live: "polite", aria_relevant: "additions",
                                if visible_events.is_empty() {
                                    div { class: "empty", "{tr(&current_locale, \"notification-empty\")}" }
                                }
                                for event in visible_events {
                                    EventCard {
                                        key: "{event.id}-{event.timestamp}-{event.event}",
                                        event: event.clone(),
                                        locale: current_locale.clone(),
                                        on_history: move |id: String| {
                                            history_id.set(Some(id.clone()));
                                            history_page.set(None);
                                            history_error.set(None);
                                            history_loading.set(true);
                                            spawn(async move { load_history(id, 0, history_page, history_error, history_loading).await; });
                                        },
                                    }
                                }
                            }
                        }
                    }
                    article { class: "card",
                        header { class: "card-head", div {
                            h2 { "{tr(&current_locale, \"summary-title\")}" }
                            p { "{tr(&current_locale, \"summary-current\")}" }
                        } }
                        div { class: "card-body",
                            div { class: "side-filters",
                                SummaryLine { label: tr(&current_locale, "summary-healthy"), value: format_number(ok_count, &current_locale), class_name: "good" }
                                SummaryLine { label: tr(&current_locale, "filter-status-failed"), value: format_number(failed_count, &current_locale), class_name: "bad" }
                                SummaryLine { label: tr(&current_locale, "filter-status-not_checked"), value: format_number(unchecked_count, &current_locale), class_name: "muted" }
                            }
                        }
                    }
                }
            }
        }

        if let Some(id) = history_id() {
            HistoryDialog {
                id,
                locale: current_locale.clone(),
                page: history_page(),
                error: history_error(),
                loading: history_loading(),
                on_close: move |_| history_id.set(None),
                on_page: move |offset| {
                    if let Some(id) = history_id() {
                        history_loading.set(true);
                        history_error.set(None);
                        spawn(async move { load_history(id, offset, history_page, history_error, history_loading).await; });
                    }
                },
                on_retry: move |_| {
                    if let Some(id) = history_id() {
                        history_loading.set(true);
                        history_error.set(None);
                        spawn(async move { load_history(id, history_page().map_or(0, |page| page.offset), history_page, history_error, history_loading).await; });
                    }
                },
            }
        }
    }
}

#[component]
fn StatusRow(status: DocStatus, locale: String, on_history: EventHandler<String>) -> Element {
    let status_class = format!("pill {}", status.status);
    let status_label = match status.status.as_str() {
        "ok" => tr(&locale, "status-ok"),
        "failed" => tr_count(
            &locale,
            "status-failed",
            status.consecutive_failures as usize,
            &[],
        ),
        _ => tr(&locale, "status-not_checked"),
    };
    let href = if status.url.starts_with("https://") || status.url.starts_with("http://") {
        Some(status.url.clone())
    } else {
        None
    };
    rsx! {
        tr {
            td {
                strong { class: "target-id", "{status.id}" }
                if let Some(href) = href {
                    a { class: "target-url", href: "{href}", target: "_blank", rel: "noopener noreferrer", "{status.url}" }
                } else {
                    span { class: "target-url", "{status.url}" }
                }
            }
            td { span { class: status_class, "{status_label}" } }
            td { "{format_time(status.last_attempted_unix_ms, &locale)}" }
            td { "{format_time(status.last_success_unix_ms, &locale)}" }
            td { "{format_time(status.last_updated_unix_ms, &locale)}" }
            td { if let Some(error) = status.last_error { span { class: "error-text", "{error}" } } else { span { class: "muted", "—" } } }
            td { button { class: "quiet", aria_label: "{tr_args(&locale, \"history-for_target\", &[(\"id\", status.id.clone())])}", onclick: move |_| on_history.call(status.id.clone()), "{tr(&locale, \"action-open\")}" } }
        }
    }
}

#[component]
fn EventCard(event: DocUpdateInfo, locale: String, on_history: EventHandler<String>) -> Element {
    let class = match event.event.as_str() {
        "poll_failed" => "event-card failed",
        "poll_recovered" => "event-card recovered",
        _ => "event-card",
    };
    let label = match event.event.as_str() {
        "poll_failed" => tr(&locale, "notification-event-poll_failed"),
        "poll_recovered" => tr(&locale, "notification-event-poll_recovered"),
        _ => tr(&locale, "notification-event-changed"),
    };
    let heading = tr_args(
        &locale,
        "notification-event_heading",
        &[("event", label.clone()), ("id", event.id.clone())],
    );
    let href = safe_http_url(&event.url);
    rsx! {
        article { class,
            div { class: "event-top",
                span { class: "event-title", "{heading}" }
                span { class: "event-time", "{event.timestamp}" }
            }
            if let Some(error) = event.error { p { class: "error-text", "{error}" } }
            div { class: "event-actions",
                if let Some(href) = href { a { href: "{href}", target: "_blank", rel: "noopener noreferrer", "{tr(&locale, \"notification-open_target\")}" } }
                else { span { class: "muted", "{event.url}" } }
                button { class: "quiet", onclick: move |_| on_history.call(event.id.clone()), "{tr(&locale, \"action-history\")}" }
            }
        }
    }
}

#[component]
fn SummaryLine(label: String, value: String, class_name: String) -> Element {
    rsx! { div { style: "display:flex;justify-content:space-between;align-items:center", span { class: "muted", "{label}" } strong { class: "{class_name}", "{value}" } } }
}

#[component]
fn HistoryDialog(
    id: String,
    locale: String,
    page: Option<HistoryPage>,
    error: Option<ApiError>,
    loading: bool,
    on_close: EventHandler<MouseEvent>,
    on_page: EventHandler<usize>,
    on_retry: EventHandler<MouseEvent>,
) -> Element {
    let newer_offset = page
        .as_ref()
        .map(|page| page.offset.saturating_sub(page.limit))
        .unwrap_or_default();
    let older_offset = page
        .as_ref()
        .map(|page| page.offset + page.limit)
        .unwrap_or_default();
    rsx! {
        div { class: "history", role: "presentation",
            section { class: "history-panel", role: "dialog", aria_modal: "true", aria_labelledby: "history-title",
                header { class: "history-head",
                    div {
                        h2 { id: "history-title", "{tr(&locale, \"history-title\")}" }
                        p { "{tr_args(&locale, \"history-for_target\", &[(\"id\", id.clone())])}" }
                    }
                    button { class: "quiet", aria_label: "{tr(&locale, \"history-close\")}", onclick: move |event| on_close.call(event), "{tr(&locale, \"history-close\")}" }
                }
                div { class: "history-content",
                    if loading { div { class: "empty", "{tr(&locale, \"history-loading\")}" } }
                    if let Some(error) = error {
                        div { class: "empty", "{tr_args(&locale, \"history-fetch_error\", &[(\"error\", api_error_message(&locale, &error))])}" }
                        button { onclick: move |event| on_retry.call(event), "{tr(&locale, \"history-retry\")}" }
                    }
                    if let Some(page) = page.as_ref() {
                        if page.entries.is_empty() { div { class: "empty", "{tr(&locale, \"history-empty\")}" } }
                        for entry in page.entries.iter() {
                            HistoryEntryView { key: "{entry.timestamp_unix_ms}", entry: entry.clone(), locale: locale.clone() }
                        }
                    }
                }
                if let Some(page) = page.as_ref() {
                    footer { class: "history-controls",
                        span { "{tr_count(&locale, \"history-page_range\", page.total, &[
                            (\"start\", format_number(page.offset + 1, &locale)),
                            (\"end\", format_number((page.offset + page.entries.len()).min(page.total), &locale)),
                        ])}" }
                        div {
                            button { disabled: !page.has_newer || loading, onclick: move |_| on_page.call(newer_offset), "{tr(&locale, \"history-newer\")}" }
                            button { disabled: !page.has_older || loading, onclick: move |_| on_page.call(older_offset), "{tr(&locale, \"history-older\")}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn HistoryEntryView(entry: ChangeHistoryEntry, locale: String) -> Element {
    let saved_content_label = if entry.content_truncated {
        tr(&locale, "history-saved_content_truncated")
    } else {
        tr(&locale, "history-saved_content")
    };
    rsx! {
        article { class: "history-entry",
            h3 { "{format_time(Some(entry.timestamp_unix_ms), &locale)}" }
            if let Some(previous) = entry.previous_content.as_deref() {
                strong {
                    if entry.previous_truncated || entry.content_truncated { "{tr(&locale, \"history-diff_truncated\")}" } else { "{tr(&locale, \"history-diff\")}" }
                }
                if let Some(lines) = history_diff(previous, &entry.content) {
                    pre { class: "history-diff",
                        for (index, line) in lines.iter().enumerate() {
                            span { key: "{index}", class: "diff-line {line.class_name}", "{line.prefix}{line.text}" }
                        }
                    }
                } else {
                    p { class: "muted", "{tr(&locale, \"history-diff_too_large\")}" }
                    strong { "{tr(&locale, \"history-before\")}" }
                    pre { "{previous}" }
                    strong { "{tr(&locale, \"history-after\")}" }
                    pre { "{entry.content}" }
                }
            } else {
                p { class: "muted", "{tr(&locale, \"history-previous_missing\")}" }
                strong { "{saved_content_label}" }
                pre { "{entry.content}" }
            }
        }
    }
}

async fn fetch_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, ApiError> {
    let request = Request::get(url).send();
    let response =
        futures_util::future::select(Box::pin(request), Box::pin(TimeoutFuture::new(10_000))).await;
    let response = match response {
        futures_util::future::Either::Left((Ok(response), _)) => response,
        futures_util::future::Either::Left((Err(_), _)) => return Err(ApiError::Request),
        futures_util::future::Either::Right(((), _)) => return Err(ApiError::Timeout),
    };
    if !response.ok() {
        return Err(ApiError::Http(response.status()));
    }
    response.json().await.map_err(|_| ApiError::InvalidResponse)
}

async fn load_history(
    id: String,
    offset: usize,
    mut page_signal: Signal<Option<HistoryPage>>,
    mut error_signal: Signal<Option<ApiError>>,
    mut loading_signal: Signal<bool>,
) {
    let url = format!(
        "/api/v1/history?id={}&limit={HISTORY_PAGE_SIZE}&offset={offset}",
        encode_component(&id)
    );
    match fetch_json::<HistoryPage>(&url).await {
        Ok(page) => page_signal.set(Some(page)),
        Err(error) => error_signal.set(Some(error)),
    }
    loading_signal.set(false);
}

fn filtered_statuses(statuses: &[DocStatus], view: &StatusView) -> Vec<DocStatus> {
    let search = view.search.to_lowercase();
    let mut results = statuses
        .iter()
        .filter(|status| view.status.is_empty() || status.status == view.status)
        .filter(|status| {
            search.is_empty()
                || status.id.to_lowercase().contains(&search)
                || status.url.to_lowercase().contains(&search)
        })
        .cloned()
        .collect::<Vec<_>>();
    match view.sort.as_str() {
        "status" => results.sort_by(|a, b| {
            status_rank(&a.status)
                .cmp(&status_rank(&b.status))
                .then_with(|| a.id.cmp(&b.id))
        }),
        "last_attempted" => results.sort_by(|a, b| {
            b.last_attempted_unix_ms
                .cmp(&a.last_attempted_unix_ms)
                .then_with(|| a.id.cmp(&b.id))
        }),
        "last_updated" => results.sort_by(|a, b| {
            b.last_updated_unix_ms
                .cmp(&a.last_updated_unix_ms)
                .then_with(|| a.id.cmp(&b.id))
        }),
        _ => results.sort_by(|a, b| a.id.cmp(&b.id)),
    }
    results
}

fn status_rank(status: &str) -> u8 {
    match status {
        "failed" => 0,
        "not_checked" => 1,
        _ => 2,
    }
}

fn safe_http_url(value: &str) -> Option<&str> {
    (value.starts_with("http://") || value.starts_with("https://")).then_some(value)
}

fn history_diff(before: &str, after: &str) -> Option<Vec<DiffLine>> {
    let before_lines = before.split('\n').collect::<Vec<_>>();
    let after_lines = after.split('\n').collect::<Vec<_>>();
    if before_lines.len().checked_mul(after_lines.len())? > 65_536 {
        return None;
    }
    let width = after_lines.len() + 1;
    let mut lcs = vec![0_u16; (before_lines.len() + 1) * width];
    for i in (0..before_lines.len()).rev() {
        for j in (0..after_lines.len()).rev() {
            let index = i * width + j;
            lcs[index] = if before_lines[i] == after_lines[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[index + 1])
            };
        }
    }

    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < before_lines.len() || j < after_lines.len() {
        if i < before_lines.len() && j < after_lines.len() && before_lines[i] == after_lines[j] {
            result.push(DiffLine {
                prefix: "  ",
                text: before_lines[i].to_owned(),
                class_name: "",
            });
            i += 1;
            j += 1;
        } else if i < before_lines.len()
            && (j == after_lines.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
        {
            result.push(DiffLine {
                prefix: "− ",
                text: before_lines[i].to_owned(),
                class_name: "diff-remove",
            });
            i += 1;
        } else {
            result.push(DiffLine {
                prefix: "+ ",
                text: after_lines[j].to_owned(),
                class_name: "diff-add",
            });
            j += 1;
        }
    }
    Some(result)
}

fn status_csv(statuses: &[DocStatus]) -> String {
    let mut csv = String::from(
        "id,url,status,last_attempted_utc,last_success_utc,last_updated_utc,last_error\r\n",
    );
    for status in statuses {
        let row = [
            status.id.clone(),
            status.url.clone(),
            status.status.clone(),
            format_utc(status.last_attempted_unix_ms),
            format_utc(status.last_success_unix_ms),
            format_utc(status.last_updated_unix_ms),
            status.last_error.clone().unwrap_or_default(),
        ];
        csv.push_str(
            &row.iter()
                .map(|field| csv_field(field))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push_str("\r\n");
    }
    csv
}

fn event_csv(events: &[DocUpdateInfo]) -> String {
    let mut csv = String::from("event,id,url,timestamp,error\r\n");
    for event in events {
        let row = [
            event.event.clone(),
            event.id.clone(),
            event.url.clone(),
            event.timestamp.clone(),
            event.error.clone().unwrap_or_default(),
        ];
        csv.push_str(
            &row.iter()
                .map(|field| csv_field(field))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push_str("\r\n");
    }
    csv
}

fn csv_field(value: &str) -> String {
    let safe = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}

fn format_utc(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return String::new();
    };
    #[cfg(target_arch = "wasm32")]
    {
        return js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(timestamp as f64))
            .to_iso_string()
            .into();
    }
    #[cfg(not(target_arch = "wasm32"))]
    timestamp.to_string()
}

fn encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn format_time(timestamp: Option<i64>, locale: &str) -> String {
    let Some(timestamp) = timestamp else {
        return "—".into();
    };
    #[cfg(target_arch = "wasm32")]
    {
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(timestamp as f64));
        let browser_locale = if locale == "ja" { "ja-JP" } else { "en-US" };
        return date
            .to_locale_string(browser_locale, &wasm_bindgen::JsValue::UNDEFINED)
            .into();
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = locale;
    #[cfg(not(target_arch = "wasm32"))]
    format!("{timestamp}")
}

fn now_unix_ms() -> i64 {
    #[cfg(target_arch = "wasm32")]
    return js_sys::Date::now() as i64;
    #[cfg(not(target_arch = "wasm32"))]
    0
}

fn load_status_view() -> StatusView {
    #[cfg(target_arch = "wasm32")]
    if let Some(value) = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(STATUS_VIEW_STORAGE_KEY).ok().flatten())
        .and_then(|value| serde_json::from_str::<StatusView>(&value).ok())
    {
        if matches!(value.status.as_str(), "" | "ok" | "failed" | "not_checked")
            && matches!(
                value.sort.as_str(),
                "id" | "status" | "last_attempted" | "last_updated"
            )
            && matches!(value.refresh_ms, 5000 | 15000 | 30000 | 60000)
        {
            return value;
        }
    }
    StatusView::default()
}

fn save_status_view(value: &str) {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(STATUS_VIEW_STORAGE_KEY, value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = value;
}

#[cfg(target_arch = "wasm32")]
fn download_csv(filename: &str, content: &str) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(&format!("\u{feff}{content}")));
    let options = BlobPropertyBag::new();
    options.set_type("text/csv;charset=utf-8");
    let Ok(blob) = Blob::new_with_str_sequence_and_options(&parts, &options) else {
        return;
    };
    let Ok(url) = Url::create_object_url_with_blob(&blob) else {
        return;
    };
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(element) = document.create_element("a") else {
        return;
    };
    let Ok(anchor) = element.dyn_into::<HtmlAnchorElement>() else {
        return;
    };
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    let _ = Url::revoke_object_url(&url);
}

#[cfg(not(target_arch = "wasm32"))]
fn download_csv(_: &str, _: &str) {}
