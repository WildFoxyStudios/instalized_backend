//! Crawler User-Agent classifier. Conservative substring match — false positives
//! just mean a human sees the (valid, fast) og: HTML instead of the redirect.

const BOT_MARKERS: &[&str] = &[
    "googlebot",
    "bingbot",
    "duckduckbot",
    "baiduspider",
    "yandexbot",
    "twitterbot",
    "facebookexternalhit",
    "facebookcatalog",
    "linkedinbot",
    "whatsapp",
    "telegrambot",
    "discordbot",
    "slackbot",
    "pinterest",
    "applebot",
    "embedly",
    "quora link preview",
    "redditbot",
];

pub fn is_bot(user_agent: &str) -> bool {
    let ua = user_agent.to_lowercase();
    BOT_MARKERS.iter().any(|m| ua.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_crawlers() {
        assert!(is_bot("Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"));
        assert!(is_bot("Twitterbot/1.0"));
        assert!(is_bot("WhatsApp/2.23.20"));
    }

    #[test]
    fn passes_humans() {
        assert!(!is_bot(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/125 Safari/537.36"
        ));
        assert!(!is_bot(""));
    }
}
