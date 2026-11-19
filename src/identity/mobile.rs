//! Lazy mobile JWGL login protocol.
//!
//! The mobile registrar (`jwglweixin.bupt.edu.cn`) is a separate undergraduate
//! backend. It accepts exactly one form POST at a fixed TLS URL with
//! `userNo`/`pwd`; this module never follows a redirect, retries, or sends a
//! password anywhere else. The caller decides whether a password is needed by
//! first reusing a bound, unexpired token.

use crate::cli::{CliError, CliResult};
use crate::transport::{Request, Response, Transport};
use serde_json::Value;
use std::fmt;

pub(super) const MOBILE_LOGIN_URL: &str = "https://jwglweixin.bupt.edu.cn/bjyddx/login";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ACCEPT: &str = "application/json, text/plain, */*";
const TOKEN_REFRESH_BUFFER_SECONDS: f64 = 120.0;
const FALLBACK_TOKEN_LIFETIME_SECONDS: f64 = 1800.0;
const MOBILE_PASSWORD_HINT: &str = "Mobile JWGL checks the JWGL password for jwgl.bupt.edu.cn, which can differ from the CAS password. This build stores only the single password in credentials.json, did not retry, and has no separate mobile-JWGL credential source.";

pub(super) struct MobileToken {
    pub(super) token: String,
    pub(super) expires_at: f64,
}

impl fmt::Debug for MobileToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MobileToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub(super) fn login(
    transport: &mut dyn Transport,
    username: &str,
    password: &str,
) -> CliResult<MobileToken> {
    if username.trim().is_empty() || password.is_empty() {
        return Err(auth_error("Mobile JWGL credentials are not configured"));
    }
    let body = form_body(username.trim(), password);
    let response = transport
        .send(Request {
            method: "POST",
            url: MOBILE_LOGIN_URL.to_owned(),
            headers: vec![
                ("User-Agent".into(), USER_AGENT.into()),
                ("Accept".into(), ACCEPT.into()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
            ],
            body,
        })
        .map_err(transport_error)?;
    interpret_response(&response)
}

/// Keep transport failures bounded to a stable, secret-free message. The
/// adapter may include request context in its own error; never forward it.
fn transport_error(error: CliError) -> CliError {
    match error.kind {
        "auth" => auth_error("Mobile JWGL login request was refused"),
        "network" => CliError::new(
            4,
            "network",
            "Mobile JWGL login network request failed",
            "Check network connectivity; no password retry was attempted.",
        ),
        _ => upstream_error("Mobile JWGL login request failed"),
    }
}

fn interpret_response(response: &Response) -> CliResult<MobileToken> {
    if matches!(response.status, 401 | 403) {
        return Err(auth_error(format!(
            "Mobile JWGL rejected authentication (HTTP {})",
            response.status
        )));
    }
    if !(200..300).contains(&response.status) {
        return Err(http_error(response.status));
    }
    let payload: Value = serde_json::from_slice(&response.body)
        .map_err(|_| upstream_error("Mobile JWGL login returned invalid JSON"))?;
    let payload = payload
        .as_object()
        .ok_or_else(|| upstream_error("Mobile JWGL login response must be a JSON object"))?;
    let code = payload.get("code");
    if code.is_none() {
        return Err(upstream_error(
            "Mobile JWGL login response has no status code",
        ));
    }
    if !code.is_some_and(success_code) {
        return Err(auth_error("Mobile JWGL rejected the saved password"));
    }
    let token = payload
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && !value
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        })
        .ok_or_else(|| auth_error("Mobile JWGL login succeeded but returned no token"))?;
    Ok(MobileToken {
        token: token.to_owned(),
        expires_at: token_expiry(token, super::unix_now()),
    })
}

fn success_code(value: &Value) -> bool {
    match value {
        Value::Number(value) => value.as_f64() == Some(1.0),
        Value::String(value) => value.trim() == "1",
        _ => false,
    }
}

fn form_body(username: &str, password: &str) -> Vec<u8> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("userNo", username);
    serializer.append_pair("pwd", password);
    serializer.finish().into_bytes()
}

/// Expiry stored for a freshly issued token. Mobile JWGL uses a millisecond
/// `exp`; opaque tokens fall back to a conservative half hour.
fn token_expiry(token: &str, now: f64) -> f64 {
    let exp = super::jwt_payload(token)
        .and_then(|payload| payload.get("exp").and_then(super::number_value));
    match exp {
        Some(exp) if exp.is_finite() => {
            let seconds = if exp > 1e12 { exp / 1000.0 } else { exp };
            seconds - TOKEN_REFRESH_BUFFER_SECONDS
        }
        _ => now + FALLBACK_TOKEN_LIFETIME_SECONDS,
    }
}

fn auth_error(message: impl Into<String>) -> CliError {
    CliError::new(3, "auth", message, MOBILE_PASSWORD_HINT)
}

fn upstream_error(message: impl Into<String>) -> CliError {
    CliError::new(
        5,
        "upstream",
        message,
        "Mobile JWGL may be unavailable or have changed its login response; no password retry was attempted.",
    )
}

fn http_error(status: u16) -> CliError {
    if status == 429 {
        CliError::new(
            5,
            "upstream",
            "Mobile JWGL login was rate limited (HTTP 429)",
            "Wait before trying again; no password retry was attempted.",
        )
    } else if (300..400).contains(&status) {
        CliError::new(
            5,
            "upstream",
            format!("Mobile JWGL login returned an unexpected redirect (HTTP {status})"),
            "The redirect was not followed and the password was not replayed.",
        )
    } else {
        CliError::new(
            5,
            "upstream",
            format!("Mobile JWGL login failed (HTTP {status})"),
            "The service may be temporarily unavailable; no password retry was attempted.",
        )
    }
}
