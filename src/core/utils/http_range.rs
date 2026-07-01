use anyhow::{Context, anyhow};

/// Parse the byte range from a `Content-Range: bytes start-end/total` header value.
/// Returns `(start, end)` on success.
pub(crate) fn parse_content_range(content_range: &str) -> Option<(u64, u64)> {
    let trimmed = content_range.trim();
    let range_part = trimmed.strip_prefix("bytes ")?;
    let (bytes_span, _total) = range_part.split_once('/')?;
    let (start_raw, end_raw) = bytes_span.split_once('-')?;
    let start = start_raw.parse::<u64>().ok()?;
    let end = end_raw.parse::<u64>().ok()?;
    Some((start, end))
}

/// Validate that a `Content-Range` response header matches the requested byte range.
pub(crate) fn validate_content_range_header(
    response: &reqwest::Response,
    requested_start: u64,
    requested_end: u64,
) -> anyhow::Result<()> {
    let header_value = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .ok_or_else(|| anyhow!("missing Content-Range header for range response"))?
        .to_str()
        .context("invalid Content-Range header value")?;

    let (actual_start, actual_end) =
        parse_content_range(header_value).ok_or_else(|| anyhow!("invalid Content-Range format"))?;
    if actual_start != requested_start || actual_end != requested_end {
        return Err(anyhow!(
            "invalid Content-Range: requested {}-{}, got {}-{}",
            requested_start,
            requested_end,
            actual_start,
            actual_end
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_range_valid() {
        assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99)));
        assert_eq!(parse_content_range("bytes 500-999/1000"), Some((500, 999)));
    }

    #[test]
    fn parse_content_range_with_whitespace() {
        assert_eq!(parse_content_range("  bytes 0-99/1000  "), Some((0, 99)));
    }

    #[test]
    fn parse_content_range_unknown_total() {
        assert_eq!(parse_content_range("bytes 0-99/*"), Some((0, 99)));
    }

    #[test]
    fn parse_content_range_invalid() {
        assert_eq!(parse_content_range(""), None);
        assert_eq!(parse_content_range("bytes"), None);
        assert_eq!(parse_content_range("bytes 0/100"), None);
        assert_eq!(parse_content_range("bytes abc-def/100"), None);
        assert_eq!(parse_content_range("none 0-99/100"), None);
    }

    #[test]
    fn parse_content_range_large_values() {
        assert_eq!(
            parse_content_range("bytes 0-4294967295/4294967296"),
            Some((0, 4_294_967_295))
        );
    }
}
