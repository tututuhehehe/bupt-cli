//! URLs observed in the electricity service's HTTPS CAS sign-in flow.
use url::Url;

pub(crate) const LANDING: &str = "https://app.bupt.edu.cn/buptdf/wap/default/chong";
const LOGIN: &str = "/uc/wap/login";
const CALLBACK: &str = "/a_bupt/api/sso/cas";
const CAS: &str = "https://auth.bupt.edu.cn/authserver/login";
const APP: &str = "https://app.bupt.edu.cn";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignIn {
    Login,
    Service,
    Ticket,
}

pub(crate) fn sign_in(raw: &str) -> Option<SignIn> {
    if raw.len() > 8192 || !raw.bytes().all(|b| b.is_ascii_graphic()) || raw.contains('\\') {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if !raw.starts_with("https://app.bupt.edu.cn/")
        || url.as_str() != raw
        || url.fragment().is_some()
    {
        return None;
    }
    let p = url.query_pairs().collect::<Vec<_>>();
    let has = |key, value| p.iter().any(|(k, v)| k == key && v == value);
    if !has("redirect", LANDING) {
        return None;
    }
    match url.path() {
        LOGIN if p.len() == 1 => Some(SignIn::Login),
        CALLBACK if has("from", "wap") => match p.len() {
            2 => Some(SignIn::Service),
            3 if p.iter().any(|(k, v)| {
                k == "ticket"
                    && v.starts_with("ST-")
                    && (4..=2048).contains(&v.len())
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            }) =>
            {
                Some(SignIn::Ticket)
            }
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn cas_service(raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    if raw.split_once('?')?.0 != CAS || url.as_str() != raw || url.fragment().is_some() {
        return None;
    }
    let fields = url.query_pairs().collect::<Vec<_>>();
    if fields.len() != 2
        || !fields
            .iter()
            .any(|(k, v)| k == "noAutoRedirect" && v == "1")
    {
        return None;
    }
    let (_, value) = fields.iter().find(|(k, _)| k == "service")?;
    (sign_in(value) == Some(SignIn::Service))
        .then(|| Url::parse(value).ok())
        .flatten()
}

pub(crate) fn matches_ticket(raw: &str, service: &Url) -> bool {
    sign_in(raw) == Some(SignIn::Ticket)
        && Url::parse(raw).is_ok_and(|url| {
            // Order and escaping can change when CAS adds the ticket, but not service fields.
            url.query_pairs()
                .filter(|(key, _)| key != "ticket")
                .all(|pair| service.query_pairs().any(|original| original == pair))
        })
}

pub(crate) fn app_redirect(raw: &str) -> Option<String> {
    // Only accept absolute HTTPS URLs and same-origin absolute paths, with no
    // actual or encoded credentials in an alternate authority.
    if raw.starts_with('/') && !raw.starts_with("//") && !raw.contains('\\') {
        Some(format!("{APP}{raw}"))
    } else if raw.starts_with("https://app.bupt.edu.cn/") {
        Some(raw.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn electricity_sign_in_rejects_other_services_and_malformed_tickets() {
        let mut callback = Url::parse(&format!("{APP}{CALLBACK}")).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("redirect", LANDING)
            .append_pair("from", "wap");
        let mut login = Url::parse(CAS).unwrap();
        login
            .query_pairs_mut()
            .append_pair("noAutoRedirect", "1")
            .append_pair("service", callback.as_str());
        assert_eq!(cas_service(login.as_str()), Some(callback.clone()));
        for invalid in [
            callback.as_str().replacen("https://", "http://", 1),
            callback
                .as_str()
                .replace("app.bupt.edu.cn/", "app.bupt.edu.cn.example.org/"),
            callback.as_str().replace("from=wap", "from=pc"),
            callback.as_str().replace("chong&", "search&"),
            format!("{callback}&redirect=https://example.org/"),
            format!("{callback}&ticket=ST-TEST&ticket=ST-OTHER"),
            format!("{callback}&ticket=ST-TEST%0A"),
        ] {
            assert_eq!(sign_in(&invalid), None);
            assert!(!matches_ticket(&invalid, &callback));
        }
        assert!(!matches_ticket(callback.as_str(), &callback));
        let ticket = format!("{callback}&ticket=ST-TEST");
        assert!(matches_ticket(&ticket, &callback));
        assert!(app_redirect("//example.org/path").is_none());
        assert!(app_redirect("http://app.bupt.edu.cn/path").is_none());
        assert!(cas_service(&format!("{login}&service={callback}")).is_none());
    }
}
