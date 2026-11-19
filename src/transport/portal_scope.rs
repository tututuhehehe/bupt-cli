//! The portal's checked TLS origins and read paths; shared by the driver and wire policy.
use url::Url;

const HOST: &str = "my.bupt.edu.cn";
const ORIGIN: &str = "https://my.bupt.edu.cn/";
const CAS: &str = "https://auth.bupt.edu.cn/authserver/login";
const CALLBACK: &str = "/system/resource/code/auth/clogin.jsp";
const LIST: &str = "/list.jsp";
pub(crate) const CARD: &str = "/system/resource/app/cuser/getwxtsA.jsp";
pub(crate) const CARD_REFERER: &str =
    "https://my.bupt.edu.cn/xs_index.jsp?urltype=tree.TreeTempUrl&wbtreeid=1541";

fn parse(raw: &str, host: &str) -> Option<Url> {
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

pub(crate) fn portal_url(raw: &str, allow_ticket: bool) -> Option<Url> {
    let mut url = parse(raw, HOST)?;
    let mut pairs = Vec::new();
    let mut tickets = 0;
    for (k, v) in url.query_pairs() {
        if k == "ticket" {
            if !allow_ticket
                || v.is_empty()
                || v.len() > 2048
                || v.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return None;
            }
            tickets += 1;
        } else {
            pairs.push((k.into_owned(), v.into_owned()));
        }
    }
    if tickets > 1 {
        return None;
    }
    let allowed = match url.path() {
        LIST => {
            pairs.len() == 4
                && [
                    ("totalpage", "30"),
                    ("PAGENUM", "0"),
                    ("urltype", "tree.TreeTempUrl"),
                ]
                .iter()
                .all(|(key, value)| pairs.iter().any(|(k, v)| k == key && v == value))
                && pairs.iter().any(|(k, v)| {
                    k == "wbtreeid" && matches!(v.as_str(), "1154" | "1221" | "1536" | "1524")
                })
        }
        CARD | CALLBACK | "/" => pairs.is_empty(),
        path => pairs.is_empty() && (path.ends_with(".htm") || path.ends_with(".html")),
    };
    if !allowed {
        return None;
    }
    // Parse without the ticket when checking the service a redirect authenticates.
    if tickets != 0 {
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
    }
    Some(url)
}

pub(crate) fn to_tls(raw: &str) -> Option<Url> {
    if let Some(suffix) = raw.strip_prefix("http://my.bupt.edu.cn/") {
        portal_url(&format!("{ORIGIN}{suffix}"), false)
    } else {
        portal_url(raw, false)
    }
}

pub(crate) fn service_from_cas(raw: &str) -> Option<Url> {
    let url = parse(raw, "auth.bupt.edu.cn")?;
    if url.path() != "/authserver/login" {
        return None;
    }
    let mut query = url.query_pairs();
    let (key, value) = query.next()?;
    (key == "service" && query.next().is_none())
        .then(|| portal_url(&value, false))
        .flatten()
}

pub(crate) fn service_matches(service: &Url, requested: &Url) -> bool {
    service == requested || (service.path() == CALLBACK && service.query().is_none())
}

pub(crate) fn cas_url(service: &Url) -> String {
    let mut url = Url::parse(CAS).expect("static CAS URL");
    url.query_pairs_mut()
        .append_pair("service", service.as_str());
    url.into()
}

pub(crate) fn matches_ticket(raw: &str, service: &Url) -> bool {
    let Some(target) = portal_url(raw, true) else {
        return false;
    };
    target.path() == service.path()
        && target.query_pairs().eq(service.query_pairs())
        && Url::parse(raw).is_ok_and(|url| url.query_pairs().any(|(k, _)| k == "ticket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_both_sides_of_the_tls_service_ticket_exchange() {
        let service = to_tls("http://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp").unwrap();
        assert_eq!(service_from_cas(&cas_url(&service)), Some(service.clone()));
        assert!(matches_ticket(
            &format!("{service}?ticket=ST-TEST"),
            &service
        ));
        for reject in [
            "http://my.bupt.edu.cn/",
            "https://my.bupt.edu.cn.example/",
            "https://my.bupt.edu.cn:443/",
            "https://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?ticket=ST-TEST&ticket=ST-TEST",
            "https://my.bupt.edu.cn/system/resource/code/auth/clogin.jsp?ticket=ST-TEST&next=/",
            "https://my.bupt.edu.cn/%2e%2e/",
            "https://my.bupt.edu.cn/other.htm?ticket=ST-TEST",
        ] {
            assert!(!matches_ticket(reject, &service));
        }
        let plaintext = cas_url(&Url::parse("http://my.bupt.edu.cn/").unwrap());
        assert!(service_from_cas(&plaintext).is_none());
    }
}
