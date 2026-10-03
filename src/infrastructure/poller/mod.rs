pub mod http_poller;
pub mod playwright_poller;

pub use self::http_poller::HttpPoller;
pub use self::playwright_poller::PlaywrightPoller;

pub(crate) fn normalize_whitespace(content: String, normalize: bool) -> String {
    if normalize {
        let (word_bytes, word_count) = content
            .split_whitespace()
            .fold((0_usize, 0_usize), |(bytes, count), word| {
                (bytes + word.len(), count + 1)
            });
        let normalized_len = word_bytes.saturating_add(word_count.saturating_sub(1));
        let mut normalized = String::with_capacity(normalized_len);
        let mut first_word = true;
        for word in content.split_whitespace() {
            if !first_word {
                normalized.push(' ');
            }
            normalized.push_str(word);
            first_word = false;
        }
        normalized
    } else {
        content
    }
}
