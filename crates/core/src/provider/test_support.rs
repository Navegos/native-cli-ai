use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

use tiny_http::{Header, Request, Response, Server, StatusCode};

use super::StreamChunk;

pub fn spawn_sse_server<F>(body: String, status: u16, assert_request: F) -> String
where
    F: FnOnce(&Request) + Send + 'static,
{
    let server = Server::http("127.0.0.1:0").expect("start mock server");
    let base_url = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => format!("http://{addr}"),
        other => panic!("unsupported listen addr: {other:?}"),
    };

    thread::spawn(move || {
        let request = server.recv().expect("receive request");
        assert_request(&request);
        let response = Response::from_string(body)
            .with_status_code(StatusCode(status))
            .with_header(
                Header::from_bytes("Content-Type", "text/event-stream")
                    .expect("content type header"),
            );
        request.respond(response).expect("send response");
    });

    base_url
}

/// Client for talking to a mock server. Ignores proxy settings, which could reroute 127.0.0.1
/// traffic and cost seconds of system proxy lookups on macOS.
pub fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build test client")
}

/// Write `response` verbatim (a complete raw HTTP response) to the first connection, then close it.
/// Unlike `spawn_sse_server` this controls headers and can send a response that ends early.
pub fn spawn_raw_http_server(response: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("start mock server");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("mock server addr")
    );

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");

        // Read the whole request first; closing with unread bytes would reset the connection.
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("read request head");
            if read == 0 || line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().expect("request content length");
            }
        }
        let mut request_body = vec![0u8; content_length];
        reader
            .read_exact(&mut request_body)
            .expect("read request body");

        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    });

    base_url
}

/// Serve a 200 event stream that promises more bytes than it sends, then close the connection.
/// A client reads `partial_body` and then hits a body read error, like a dropped connection.
pub fn spawn_truncated_sse_server(partial_body: String) -> String {
    spawn_raw_http_server(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{partial_body}",
        partial_body.len() + 4096
    ))
}

pub async fn collect_chunks(mut rx: tokio::sync::mpsc::Receiver<StreamChunk>) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    while let Some(chunk) = rx.recv().await {
        let done = matches!(chunk, StreamChunk::Done);
        chunks.push(chunk);
        if done {
            break;
        }
    }
    chunks
}

/// Variant names in order, for asserting the shape of a stream without matching payloads.
pub fn chunk_kinds(chunks: &[StreamChunk]) -> Vec<&'static str> {
    chunks
        .iter()
        .map(|chunk| match chunk {
            StreamChunk::TextDelta(_) => "text",
            StreamChunk::ReasoningDelta(_) => "reasoning",
            StreamChunk::ToolUse(_) => "tool_use",
            StreamChunk::InvalidToolUse { .. } => "invalid_tool_use",
            StreamChunk::Usage { .. } => "usage",
            StreamChunk::Finish { .. } => "finish",
            StreamChunk::Failed(_) => "failed",
            StreamChunk::Done => "done",
        })
        .collect()
}
