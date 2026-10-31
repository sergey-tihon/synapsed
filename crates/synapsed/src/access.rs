//! Host and Origin checks protect against hostile browser origins, not local users.
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::{IpAddr, SocketAddr};
use synapsed_api::{ApiError, ErrorCode, ErrorResponse};

#[derive(Clone, Debug)]
pub struct AccessPolicy {
    bound: SocketAddr,
    allowed_origins: Vec<String>,
}
impl AccessPolicy {
    pub fn new(bound: SocketAddr, allowed_origins: Vec<String>) -> Result<Self, ApiError> {
        if !bound.ip().is_loopback() {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Only literal loopback listen addresses are supported.",
            ));
        }
        for origin in &allowed_origins {
            validate_origin(origin)?;
        }
        Ok(Self {
            bound,
            allowed_origins,
        })
    }
    pub fn check(&self, headers: &HeaderMap) -> Result<Option<HeaderValue>, ApiError> {
        if headers.get_all(header::HOST).iter().count() != 1 {
            return Err(ApiError::new(
                ErrorCode::ForbiddenHost,
                "One valid local Host header is required.",
            ));
        }
        let host = headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok())
            .and_then(|host| host.parse::<Authority>().ok())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ForbiddenHost,
                    "Host is not the bound local server.",
                )
            })?;
        let hostname = host.host().trim_start_matches('[').trim_end_matches(']');
        if host.as_str().contains('@')
            || host.port_u16().unwrap_or(80) != self.bound.port()
            || (host.as_str().len() != host.host().len() && host.port_u16().is_none())
            || !(hostname.eq_ignore_ascii_case("localhost")
                || hostname
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip == self.bound.ip()))
        {
            return Err(ApiError::new(
                ErrorCode::ForbiddenHost,
                "Host is not the bound local server.",
            ));
        }
        if headers.get_all(header::ORIGIN).iter().count() > 1 {
            return Err(ApiError::new(
                ErrorCode::ForbiddenOrigin,
                "Only one browser Origin is accepted.",
            ));
        }
        let Some(origin) = headers.get(header::ORIGIN) else {
            return Ok(None);
        };
        let value = origin.to_str().map_err(|_| {
            ApiError::new(ErrorCode::ForbiddenOrigin, "Browser Origin is not allowed.")
        })?;
        validate_origin(value).map_err(|_| {
            ApiError::new(ErrorCode::ForbiddenOrigin, "Browser Origin is not allowed.")
        })?;
        // Origin is exact, including the authority used in this request, not every loopback port.
        let same_bound_origin = value
            .strip_prefix("http://")
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host.as_str()));
        if !same_bound_origin && !self.allowed_origins.iter().any(|allowed| allowed == value) {
            return Err(ApiError::new(
                ErrorCode::ForbiddenOrigin,
                "Browser Origin is not allowed.",
            ));
        }
        Ok(Some(origin.clone()))
    }
}
pub fn validate_origin(origin: &str) -> Result<(), ApiError> {
    let invalid = || {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "Allowed origins must be exact loopback HTTP or HTTPS origins.",
        )
    };
    let (scheme, authority) = origin.split_once("://").ok_or_else(invalid)?;
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority.contains(['/', '?', '#', '@', '*'])
    {
        return Err(invalid());
    }
    let authority: Authority = authority.parse().map_err(|_| invalid())?;
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if !(host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
    {
        return Err(invalid());
    }
    if authority.as_str().len() != authority.host().len() && authority.port_u16().is_none() {
        return Err(invalid());
    }
    Ok(())
}
pub async fn enforce(State(policy): State<AccessPolicy>, request: Request, next: Next) -> Response {
    let origin = match policy.check(request.headers()) {
        Ok(origin) => origin,
        Err(error) => return error_response(error),
    };
    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}
pub fn error_response(error: ApiError) -> Response {
    let status = StatusCode::from_u16(error.code.http_status()).expect("API status is valid");
    (status, axum::Json(ErrorResponse { error })).into_response()
}
pub async fn preflight(headers: HeaderMap) -> Response {
    if headers
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .is_some_and(|method| method != "GET" && method != "POST")
    {
        return error_response(ApiError::new(
            ErrorCode::InvalidRequest,
            "Preflight method is unsupported.",
        ));
    }
    if headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .is_some_and(|headers| {
            !headers.to_str().is_ok_and(|headers| {
                headers
                    .split(',')
                    .all(|header| header.trim().eq_ignore_ascii_case("content-type"))
            })
        })
    {
        return error_response(ApiError::new(
            ErrorCode::InvalidRequest,
            "Preflight headers are unsupported.",
        ));
    }
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "content-type"),
        ],
    )
        .into_response()
}
