//! Build-time embedded static CSR assets; never read the source tree at runtime.
use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use include_dir::{Dir, include_dir};

static ASSETS: Dir<'static> = include_dir!("$SYNAPSED_WEB_DIR");

pub(crate) fn serve(request: Request) -> Response {
    let path = request.uri().path();
    // Unknown API routes never receive HTML, even if a build happened to contain
    // a matching filename. No URL decoding or filesystem normalization is used.
    if path == "/api"
        || path.starts_with("/api/")
        || path.contains('%')
        || path.contains('\\')
        || (request.method() != Method::GET && request.method() != Method::HEAD)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let asset_path = if path == "/" {
        "index.html"
    } else {
        let Some(relative) = path.strip_prefix('/') else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if relative
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        relative
    };
    let Some(file) = ASSETS.get_file(asset_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(asset_path).first_or_octet_stream();
    let mut response = if request.method() == Method::HEAD {
        Body::empty().into_response()
    } else {
        Body::from(Bytes::from_static(file.contents())).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        mime.as_ref().parse().expect("MIME header"),
    );
    headers.insert(header::CONTENT_LENGTH, file.contents().len().into());
    headers.insert(
        header::CACHE_CONTROL,
        "no-cache".parse().expect("static header"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().expect("static header"),
    );
    response
}
