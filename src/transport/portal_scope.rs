//! The portal's checked read origins. `my.bupt.edu.cn` answers plain HTTP only
//! (TCP 443 is closed); CAS password entry stays on HTTPS. HTTP is allowed for
//! this one host and the read paths below, and this module never attaches a
//! credential.
use url::Url;

const HOST: &str = "my.bupt.edu.cn";
const CAS: &str = "https://auth.bupt.edu.cn/authserver/login";
pub(crate) const CALLBACK: &str = "/system/resource/code/auth/clogin.jsp";
const LIST: &str = "/list.jsp";
const SEARCH: &str = "/fz_ssjg.jsp";
pub(crate) const DOWNLOAD: &str = "/system/_content/download.jsp";
pub(crate) const CARD: &str = "/system/resource/app/cuser/getwxtsA.jsp";
pub(crate) const CARD_REFERER: &str =
    "http://my.bupt.edu.cn/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541";

/// Fold the one portal host onto canonical `http://my.bupt.edu.cn` with no
/// explicit port. `https://` on this host and an explicit `:80` are the same
/// origin; any other port or host is rejected.
fn normalized(raw: &str) -> Option<String> {
    if raw.len() > 8192
        || raw.contains(['\\', '#', '@'])
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let rest = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))?;
    let rest = rest.strip_prefix(HOST)?;
    let rest = if let Some(rest) = rest.strip_prefix(":80") {
        rest
    } else if rest.starts_with(':') {
        return None;
    } else {
        rest
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')) {
        return None;
    }
    Some(format!("http://{HOST}{rest}"))
}

fn parse_http(raw: &str) -> Option<Url> {
    if !raw.starts_with("http://") {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let path = url.path().to_ascii_lowercase();
    (url.scheme() == "http"
        && url.host_str() == Some(HOST)
        && url.port().is_none()
        && url.as_str() == raw
        && !path.contains("//")
        && !path.contains("%2f")
        && !path.contains("%5c"))
    .then_some(url)
}

fn parse_https(raw: &str, host: &str) -> Option<Url> {
    let rest = raw.strip_prefix("https://")?;
    if rest.split('/').next() != Some(host)
        || raw.len() > 8192
        || raw.contains(['\\', '#'])
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let path = url.path().to_ascii_lowercase();
    (url.host_str() == Some(host)
        && url.as_str() == raw
        && !path.contains("//")
        && !path.contains("%2f")
        && !path.contains("%5c"))
    .then_some(url)
}

/// Accept a portal read URL, including this host's HTTPS form and an explicit
/// port 80. A service ticket may appear only on the login callback, and the
/// returned URL is the canonical form without the default port or the ticket.
pub(crate) fn portal_url(raw: &str, allow_ticket: bool) -> Option<Url> {
    let normalized = normalized(raw)?;
    let mut url = parse_http(&normalized)?;
    let mut pairs = Vec::new();
    let mut tickets = 0;
    for (key, value) in url.query_pairs() {
        if key == "ticket" {
            if !allow_ticket || !valid_ticket(&value) {
                return None;
            }
            tickets += 1;
        } else {
            pairs.push((key.into_owned(), value.into_owned()));
        }
    }
    if tickets > 1 {
        return None;
    }
    let allowed = match url.path() {
        LIST => tickets == 0 && list_query(&pairs),
        SEARCH => tickets == 0 && search_query(&pairs),
        path if content_path(path) => tickets == 0 && content_query(&pairs),
        DOWNLOAD => tickets == 0 && download_query(&pairs),
        CALLBACK => callback_query(&pairs),
        CARD | "/" => tickets == 0 && pairs.is_empty(),
        path => {
            tickets == 0 && pairs.is_empty() && (path.ends_with(".htm") || path.ends_with(".html"))
        }
    };
    if !allowed {
        return None;
    }
    if tickets != 0 {
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
    }
    Some(url)
}

/// The portal URL to send. An already-approved HTTP URL is kept verbatim,
/// including an explicit port 80: the portal's CAS service is registered with
/// that port, and dropping it makes the ticket callback a dead page. HTTPS on
/// this host is rewritten to HTTP because nothing listens on 443.
pub(crate) fn canonical_request(raw: &str) -> Option<String> {
    portal_url(raw, true)?;
    if raw.starts_with("http://") {
        return Some(raw.to_string());
    }
    normalized(raw)
}

/// The portal service advertised by a CAS redirect, kept verbatim so the
/// ticket is requested for the same string the portal registered.
pub(crate) fn service_from_cas(raw: &str) -> Option<String> {
    let url = parse_https(raw, "auth.bupt.edu.cn")?;
    if url.path() != "/authserver/login" {
        return None;
    }
    let mut query = url.query_pairs();
    let (key, value) = query.next()?;
    if key != "service" || query.next().is_some() {
        return None;
    }
    portal_url(&value, false)?;
    Some(value.into_owned())
}

/// The portal signs in through its own callback, not by echoing the original
/// page. The callback may carry the site owner id and nothing else. There is no
/// "requested page" argument on purpose: the portal never echoes it, so tying
/// the two would be a check that only looks like one.
pub(crate) fn service_matches(service: &str) -> bool {
    portal_url(service, false).is_some_and(|url| {
        url.path() == CALLBACK && url.query_pairs().all(|(key, _)| key == "owner")
    })
}

pub(crate) fn cas_url(service: &str) -> String {
    let mut url = Url::parse(CAS).expect("static CAS URL");
    url.query_pairs_mut().append_pair("service", service);
    url.into()
}

pub(crate) fn matches_ticket(raw: &str, service: &str) -> bool {
    let Some(target) = portal_url(raw, true) else {
        return false;
    };
    let Some(service) = portal_url(service, false) else {
        return false;
    };
    target.path() == service.path()
        && target.query_pairs().eq(service.query_pairs())
        && normalized(raw).is_some_and(|url| {
            Url::parse(&url).is_ok_and(|url| url.query_pairs().any(|(key, _)| key == "ticket"))
        })
}

fn list_query(pairs: &[(String, String)]) -> bool {
    pairs.len() == 4
        && [
            ("totalpage", "30"),
            ("PAGENUM", "0"),
            ("urltype", "tree.TreeTempUrl"),
        ]
        .iter()
        .all(|(key, value)| pairs.iter().any(|(k, v)| k == key && v == value))
        && pairs.iter().any(|(key, value)| {
            key == "wbtreeid" && matches!(value.as_str(), "1154" | "1221" | "1536" | "1524")
        })
}

fn search_query(pairs: &[(String, String)]) -> bool {
    const KEYS: [&str; 10] = [
        "a218493p",
        "a218493c",
        "wbtreeid",
        "wbnewsfield",
        "Find",
        "entrymode",
        "INTEXT",
        "INTEXT2",
        "news_search_code",
        "wbtreeids",
    ];
    pairs.len() == KEYS.len()
        && KEYS
            .iter()
            .all(|key| pairs.iter().any(|(actual, _)| actual == key))
        && value(pairs, "a218493p") == Some("1")
        && value(pairs, "a218493c") == Some("10")
        && value(pairs, "wbtreeid") == Some("1154")
        && value(pairs, "wbnewsfield") == Some("wbtitle")
        && value(pairs, "Find") == Some("find")
        && value(pairs, "entrymode") == Some("2")
        && value(pairs, "INTEXT") == Some("")
        && value(pairs, "news_search_code") == Some("")
        && value(pairs, "INTEXT2").is_some_and(valid_search_token)
        && value(pairs, "wbtreeids").is_some_and(valid_tree_ids)
}

fn content_path(path: &str) -> bool {
    let Some(name) = path.strip_prefix('/') else {
        return false;
    };
    !name.contains('/') && name.ends_with("_content.jsp") && name.len() <= 64
}

fn content_query(pairs: &[(String, String)]) -> bool {
    pairs.len() == 3
        && value(pairs, "urltype") == Some("news.NewsContentUrl")
        && value(pairs, "wbtreeid").is_some_and(|id| digits(id, 1, 6))
        && value(pairs, "wbnewsid").is_some_and(|id| digits(id, 1, 12))
}

/// The attachment link the notice page itself renders. The portal refuses the
/// file unless the request also carries a portal page as `Referer`, which the
/// notice commands set; this check covers the destination only.
fn download_query(pairs: &[(String, String)]) -> bool {
    pairs.len() == 3
        && value(pairs, "urltype") == Some("news.DownloadAttachUrl")
        && value(pairs, "owner").is_some_and(|id| digits(id, 1, 16))
        && value(pairs, "wbfileid").is_some_and(|id| digits(id, 1, 12))
}

fn callback_query(pairs: &[(String, String)]) -> bool {
    pairs.is_empty()
        || (pairs.len() == 1 && value(pairs, "owner").is_some_and(|id| digits(id, 1, 16)))
}

fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(actual, _)| actual == key)
        .map(|(_, text)| text.as_str())
}

fn valid_ticket(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2048
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn valid_search_token(value: &str) -> bool {
    (4..=344).contains(&value.len())
        && value.len().is_multiple_of(4)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        && !value.trim_end_matches('=').contains('=')
}

fn valid_tree_ids(value: &str) -> bool {
    let mut count = 0;
    !value.is_empty()
        && value.len() <= 4000
        && value.split(',').all(|id| {
            count += 1;
            digits(id, 1, 6)
        })
        && count <= 400
}

fn digits(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_the_http_service_ticket_exchange() {
        let service = canonical_request(
            "http://my.bupt.edu.cn:80/system/resource/code/auth/clogin.jsp?owner=1664271694",
        )
        .unwrap();
        assert_eq!(
            service,
            "http://my.bupt.edu.cn:80/system/resource/code/auth/clogin.jsp?owner=1664271694"
        );
        assert_eq!(service_from_cas(&cas_url(&service)), Some(service.clone()));
        assert!(service_matches(&service));
        // A notice page or an attachment link is not the callback, so it cannot
        // be the service the portal signed in for.
        assert!(!service_matches(
            "http://my.bupt.edu.cn/xntz_content.jsp?urltype=news.NewsContentUrl&wbtreeid=2149&wbnewsid=144119"
        ));
        assert!(!service_matches(
            "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1&wbfileid=2"
        ));
        assert!(!service_matches(
            "http://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?owner=1&ticket=ST-TEST"
        ));
        assert!(matches_ticket(
            &format!("http://my.bupt.edu.cn:80{CALLBACK}?owner=1664271694&ticket=ST-TEST"),
            &service
        ));
        for reject in [
            "http://my.bupt.edu.cn/",
            "https://my.bupt.edu.cn.example/",
            "http://my.bupt.edu.cn:8080/",
            "http://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?ticket=ST-TEST&ticket=ST-TEST",
            "http://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?ticket=ST-TEST&next=/",
            "http://my.bupt.edu.cn/%2e%2e/",
            "http://my.bupt.edu.cn/other.htm?ticket=ST-TEST",
            "https://auth.bupt.edu.cn/authserver/login?service=http://evil.example/",
        ] {
            assert!(!matches_ticket(reject, &service), "{reject}");
        }
    }

    #[test]
    fn accepts_the_attachment_link_and_rejects_a_widened_query() {
        let attachment = "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628";
        assert_eq!(canonical_request(attachment).as_deref(), Some(attachment));
        for reject in [
            // A file id or owner that is not numeric is not this form.
            "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=abc",
            // The shape is pinned: no extra or missing fields.
            "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694",
            "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628&path=../../etc/passwd",
            "http://my.bupt.edu.cn/system/_content/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628&ticket=ST-TEST",
            "http://my.bupt.edu.cn/download.jsp?urltype=news.DownloadAttachUrl&owner=1664271694&wbfileid=3251628",
            "http://my.bupt.edu.cn/system/_content/download.jsp",
        ] {
            assert!(canonical_request(reject).is_none(), "{reject}");
        }
    }

    #[test]
    fn accepts_the_notice_search_form_and_rejects_a_widened_query() {
        let mut url = Url::parse("http://my.bupt.edu.cn/fz_ssjg.jsp").unwrap();
        url.query_pairs_mut()
            .append_pair("a218493p", "1")
            .append_pair("a218493c", "10")
            .append_pair("wbtreeid", "1154")
            .append_pair("wbnewsfield", "wbtitle")
            .append_pair("Find", "find")
            .append_pair("entrymode", "2")
            .append_pair("INTEXT", "")
            .append_pair("INTEXT2", "5aWW5a2m6YeR")
            .append_pair("news_search_code", "")
            .append_pair("wbtreeids", "1155,1158");
        assert_eq!(
            canonical_request(url.as_str()).as_deref(),
            Some(url.as_str())
        );
        let widened = url.as_str().replace("wbtreeid=1154", "wbtreeid=1221");
        assert!(canonical_request(&widened).is_none());
    }
}
