//! Minimal HTTP server for serving frame images directly to the frontend.
//! Like Screenpipe's localhost:3030 — browser loads images natively, no IPC.
//! GET /frame?path=/path/to/image.jpg → returns image bytes with correct Content-Type

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;

pub fn start(port: u16) -> Result<(), String> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).map_err(|e| format!("Bind failed: {}", e))?;
    log::info!("MindScope: Image server running on http://{}", addr);

    for stream in listener.incoming() {
        if let Ok(mut stream) = stream {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(&stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() { return; }

                // Parse: GET /frame?path=<encoded_path> HTTP/1.1
                let path = extract_path_param(&request_line);

                if let Some(file_path) = path {
                    if let Ok(bytes) = std::fs::read(&file_path) {
                        let content_type = if file_path.ends_with(".jpg") || file_path.ends_with(".jpeg") {
                            "image/jpeg"
                        } else if file_path.ends_with(".webp") {
                            "image/webp"
                        } else if file_path.ends_with(".png") {
                            "image/png"
                        } else {
                            "application/octet-stream"
                        };

                        let response = format!(
                            "HTTP/1.1 200 OK\r\n\
                            Content-Type: {}\r\n\
                            Content-Length: {}\r\n\
                            Access-Control-Allow-Origin: *\r\n\
                            Cache-Control: public, max-age=31536000, immutable\r\n\
                            \r\n",
                            content_type,
                            bytes.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(&bytes);
                    } else {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    }
                } else {
                    let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
                }
            });
        }
    }
    Ok(())
}

fn extract_path_param(request_line: &str) -> Option<String> {
    // GET /frame?path=%2FUsers%2F... HTTP/1.1
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 { return None; }

    let uri = parts[1];
    if let Some(query_start) = uri.find("?path=") {
        let encoded = &uri[query_start + 6..];
        // URL decode
        Some(url_decode(encoded))
    } else {
        None
    }
}

fn url_decode(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    result
}
