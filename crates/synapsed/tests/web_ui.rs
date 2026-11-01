#![cfg(feature = "web-ui")]

mod common;

use common::start;
use reqwest::StatusCode;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

async fn raw_status(address: std::net::SocketAddr, path: &str) -> u16 {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).await.unwrap();
    status
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}
fn hashed_asset_path(html: &str, extension: &str) -> String {
    let path = html
        .split('"')
        .find(|part| part.starts_with("/assets/") && part.ends_with(extension))
        .expect("built HTML should reference an asset with the requested extension");
    let filename = path.rsplit('/').next().unwrap();
    let stem = filename.strip_suffix(extension).unwrap();
    let bytes = stem.as_bytes();
    assert!(
        bytes.len() >= 9,
        "asset filename should include a content hash"
    );
    let hash_start = bytes.len() - 8;
    assert_eq!(bytes[hash_start - 1], b'-');
    assert!(
        bytes[hash_start..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    );
    path.to_owned()
}

#[tokio::test]
async fn serves_html_hashed_assets_and_favicon_with_cache_headers() {
    let mut fixture = start(&[]).await;
    let base = fixture.base();

    let index = fixture.http.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert!(
        index
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert_eq!(
        index.headers().get(reqwest::header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    let index_body = index.text().await.unwrap();

    let javascript_path = hashed_asset_path(&index_body, ".js");
    let javascript = fixture
        .http
        .get(format!("{base}{javascript_path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(javascript.status(), StatusCode::OK);
    let javascript_type = javascript
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        javascript_type.starts_with("application/javascript")
            || javascript_type.starts_with("text/javascript"),
        "unexpected JavaScript MIME type: {javascript_type}"
    );
    assert_eq!(
        javascript
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .unwrap(),
        "public, max-age=31536000, immutable"
    );

    let favicon = fixture
        .http
        .get(format!("{base}/favicon.svg"))
        .send()
        .await
        .unwrap();
    assert_eq!(favicon.status(), StatusCode::OK);
    assert_eq!(
        favicon
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap(),
        "image/svg+xml"
    );
    assert!(favicon.text().await.unwrap().contains("<svg"));

    let get = fixture.http.get(format!("{base}/")).send().await.unwrap();
    let expected_length = get
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("GET should have Content-Length");
    let head = fixture.http.head(format!("{base}/")).send().await.unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let head_length = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    assert_eq!(head_length, Some(expected_length));
    assert!(head.bytes().await.unwrap().is_empty());

    fixture.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_assets_unknown_api_and_encoded_traversal_return_not_found() {
    let mut fixture = start(&[]).await;
    let base = fixture.base();

    for path in [
        "/missing-asset.js",
        "/api/v1/not-a-route",
        "/api/not-a-route",
        "/assets%2findex.js",
        "/..%2findex.html",
        "/%252e%252e/index.html",
        "/assets%5cindex.js",
    ] {
        let response = fixture
            .http
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path: {path}");
    }

    for path in ["/%2e%2e/index.html", "/a/../b"] {
        assert_eq!(
            raw_status(fixture.server.address, path).await,
            404,
            "path: {path}"
        );
    }
    fixture.server.shutdown().await.unwrap();
}
