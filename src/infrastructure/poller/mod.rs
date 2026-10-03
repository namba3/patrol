pub mod http_poller;
pub mod playwright_poller;

pub use self::http_poller::HttpPoller;
pub use self::playwright_poller::PlaywrightPoller;

pub(crate) fn normalize_whitespace(content: String, normalize: bool) -> String {
    if normalize {
        content.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        content
    }
}
