//! HTTP response sanitization and truncation tests.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use wintermute::providers::{check_http_response, ProviderError};

async fn serve_once(status_line: &str, body: &str) -> String {
    let listener_result = TcpListener::bind("127.0.0.1:0").await;
    assert!(listener_result.is_ok());
    let listener = match listener_result {
        Ok(listener) => listener,
        Err(err) => panic!("listener should bind: {err}"),
    };

    let addr_result = listener.local_addr();
    assert!(addr_result.is_ok());
    let addr = match addr_result {
        Ok(addr) => addr,
        Err(err) => panic!("listener should expose local addr: {err}"),
    };

    let status_line_owned = status_line.to_owned();
    let body_owned = body.to_owned();
    tokio::spawn(async move {
        let accepted = listener.accept().await;
        if let Ok((mut socket, _)) = accepted {
            let mut read_buf = [0_u8; 1024];
            let _ = socket.read(&mut read_buf).await;

            let response = format!(
                "HTTP/1.1 {status_line_owned}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_owned}",
                body_owned.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    format!("http://{addr}/")
}

#[tokio::test]
async fn check_http_response_redacts_token_like_values() {
    let raw_token = "ghp_abcdefghijklmnopqrstuvwxyz1234";
    let body = format!("error token={raw_token}");
    let url = serve_once("500 Internal Server Error", &body).await;

    let response_result = reqwest::get(url).await;
    assert!(response_result.is_ok());
    let response = match response_result {
        Ok(response) => response,
        Err(err) => panic!("request should complete: {err}"),
    };

    let checked = check_http_response(response).await;
    assert!(checked.is_err());

    let err = match checked {
        Ok(_) => panic!("response should fail on non-success status"),
        Err(err) => err,
    };

    match err {
        ProviderError::HttpStatus { body, .. } => {
            assert!(!body.contains(raw_token));
            assert!(body.contains("[REDACTED]"));
        }
        other => panic!("expected http status error, got: {other}"),
    }
}

#[tokio::test]
async fn check_http_response_truncates_long_error_body() {
    let body = "x".repeat(400);
    let url = serve_once("500 Internal Server Error", &body).await;

    let response_result = reqwest::get(url).await;
    assert!(response_result.is_ok());
    let response = match response_result {
        Ok(response) => response,
        Err(err) => panic!("request should complete: {err}"),
    };

    let checked = check_http_response(response).await;
    assert!(checked.is_err());

    let err = match checked {
        Ok(_) => panic!("response should fail on non-success status"),
        Err(err) => err,
    };

    match err {
        ProviderError::HttpStatus { body, .. } => {
            assert!(body.ends_with("...[truncated]"));
        }
        other => panic!("expected http status error, got: {other}"),
    }
}
