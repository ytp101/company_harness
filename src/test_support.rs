//! Tiny HTTP/1.1 fixture server. Tests exercise reqwest and the real wire protocol.
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
pub type Requests = Vec<(String, Value)>;
pub async fn server(responses: Vec<(u16, Value)>) -> (String, JoinHandle<Requests>) {
    let count = responses.len();
    let mut responses = responses.into_iter();
    server_with(count, move |_| responses.next().unwrap()).await
}
pub async fn server_with<F>(count: usize, mut handler: F) -> (String, JoinHandle<Requests>)
where
    F: FnMut(&Value) -> (u16, Value) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for _ in 0..count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![];
            let header_end = loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
            let len = header
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while bytes.len() < header_end + len {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let request = if len == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[header_end..header_end + len]).unwrap()
            };
            let (status, body) = handler(&request);
            requests.push((header.lines().next().unwrap().to_string(), request));
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (endpoint, task)
}
