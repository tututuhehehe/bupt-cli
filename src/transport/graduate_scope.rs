//! Verified HTTPS URL shapes for the graduate system (yjxt).
//!
//! Production graduate reads are browserless: CAS issues a ticket for the
//! HTTPS callback, the site asks once more with its legacy HTTP service name,
//! and both observed landing pages must be visited before real data returns.
//! Only the ticketed callback and in-system `.aspx` pages with bounded,
//! already-escaped query strings are accepted; nothing is sent over plain
//! HTTP and no site challenge script is executed.

use url::Url;

const APP: &str = "yjxt.bupt.edu.cn";

fn parse(raw: &str) -> Option<Url> {
    if raw.len() > 8192
        || !raw.bytes().all(|b| b.is_ascii_graphic())
        || raw.contains('\\')
        || !raw.starts_with("https://")
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let authority = raw
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    // The parsed form may re-encode query characters; callers must send
    // `url.as_str()` so the transport sees a canonical, re-checkable URL.
    (url.host_str() == Some(APP)
        && authority == APP
        && url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

fn safe_page_path(path: &str) -> bool {
    let Some(rest) = path
        .strip_prefix("/Gstudent/")
        .or_else(|| path.strip_prefix("/PublicPage/"))
    else {
        return false;
    };
    if rest.is_empty() || rest.len() > 80 || !rest.ends_with(".aspx") {
        return false;
    }
    // The menu nests some pages one subdirectory deep. Each segment must be
    // a plain page name; empty segments and traversal names are refused.
    rest.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    })
}

/// Validate the raw query string without decoding, so already-escaped values
/// of any script or name stay exactly as the school page emitted them.
fn safe_raw_query(raw: &str) -> bool {
    let Some(query) = raw.split_once('?').map(|(_, query)| query) else {
        return true;
    };
    if query.is_empty() {
        return false;
    }
    for field in query.split('&').take(9) {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || value.len() > 2048
            || !value.bytes().all(|b| b.is_ascii_graphic())
        {
            return false;
        }
    }
    // `take(9)` allows at most 8 pairs; a ninth field must not exist.
    query.split('&').count() <= 8
}

/// The CAS ticket callback: exactly one `ticket` pair and nothing else.
pub(crate) fn ticketed_callback(raw: &str) -> Option<Url> {
    let url = parse(raw)?;
    if url.path() != "/ULogin.aspx" {
        return None;
    }
    let query = url.as_str().split_once('?')?.1;
    let (key, value) = query.split_once('=')?;
    (key == "ticket"
        && value.starts_with("ST-")
        && (4..2048).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !query.contains('&'))
    .then_some(url)
}

/// An in-system graduate page reached through the verified sign-in chain.
pub(crate) fn approved_page(raw: &str) -> Option<Url> {
    let url = parse(raw)?;
    safe_page_path(url.path())
        .then(|| safe_raw_query(raw).then_some(url))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback(ticket: &str) -> String {
        format!("https://{APP}/ULogin.aspx?ticket={ticket}")
    }

    fn page(path: &str, query: &str) -> String {
        format!("https://{APP}{path}?{query}")
    }

    #[test]
    fn ticketed_callback_accepts_one_cas_ticket_and_nothing_else() {
        assert!(ticketed_callback(&callback("ST-abc_123-456")).is_some());
        for invalid in [
            callback("st-lowercase"),
            callback("ST-a&other=1"),
            callback(""),
            format!("https://{APP}/ULogin.aspx"),
            page("/ULogin.aspx", "ticket=ST-a/b"),
            "http://yjxt.bupt.edu.cn/ULogin.aspx?ticket=ST-a".to_owned(),
            "https://yjxt.bupt.edu.cn:8443/ULogin.aspx?ticket=ST-a".to_owned(),
            "https://yjxt.bupt.edu.cn/ULogin.aspx?ticket=ST-a#x".to_owned(),
            "https://user@yjxt.bupt.edu.cn/ULogin.aspx?ticket=ST-a".to_owned(),
            "https://yjxt.bupt.edu.cn.example.org/ULogin.aspx?ticket=ST-a".to_owned(),
        ] {
            assert!(ticketed_callback(&invalid).is_none(), "accepted {invalid}");
        }
    }

    #[test]
    fn approved_pages_stay_on_verified_paths_with_escaped_queries() {
        assert!(approved_page(&page("/Gstudent/LeftMenu.aspx", "menu=00")).is_some());
        assert!(approved_page(&page("/Gstudent/Default.aspx", "uid=abc123")).is_some());
        assert!(
            approved_page(&page("/Gstudent/DefaultN.aspx", "eid=a.b_c-1&x=%E4%B8%AD")).is_some()
        );
        assert!(approved_page("https://yjxt.bupt.edu.cn/Gstudent/xskbcx.aspx").is_some());
        // The live menu nests schedule and exam pages one subdirectory deep.
        assert!(approved_page(&page("/Gstudent/Kbcx/xskbcx.aspx", "EID=abc==")).is_some());
        assert!(approved_page(&page("/PublicPage/TermCalender.aspx", "EID=abc==&UID=")).is_some());
        // A query character that URL serialization re-encodes is accepted in
        // the original form; the caller must send the parsed `as_str()` form.
        let reencoded = approved_page(&page("/Gstudent/xskbcx.aspx", "EID=a\"b=="))
            .expect("query re-encoding must not reject a menu page");
        assert_eq!(
            reencoded.as_str(),
            "https://yjxt.bupt.edu.cn/Gstudent/xskbcx.aspx?EID=a%22b=="
        );
        for invalid in [
            page("/Gstudent/../Secret.aspx", "a=1"),
            // `Url::parse` normalizes `.` and empty segments away, so those
            // raw forms cannot reach a request; the wire layer still refuses
            // any non-canonical URL that does not round-trip.
            page("/Gstudent/sub/dir/evil.aspx?x", "a=1"),
            page("/Gstudent/evil.aspx", "a=b&c=d&e=f&g=h&i=j&k=l&m=n&o=p&q=r"),
            page("/Other/Default.aspx", "a=1"),
            "http://yjxt.bupt.edu.cn/Gstudent/Default.aspx?uid=1".to_owned(),
            "https://yjxt.bupt.edu.cn/Gstudent/Default.aspx?uid=1#f".to_owned(),
            page("/Gstudent/evil.aspx", "a=1 "),
        ] {
            assert!(approved_page(&invalid).is_none(), "accepted {invalid}");
        }
    }
}
