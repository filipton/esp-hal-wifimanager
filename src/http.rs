use crate::structs::WmInnerSignals;
use alloc::{format, rc::Rc, vec::Vec};
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Stack};
use embassy_time::{Duration, Timer};

const WEB_TASK_POOL_SIZE: usize = 2;
const HTTP_BUFFER_SIZE: usize = 2048;

struct HttpRequest<'a> {
    method: &'a str,
    path: &'a str,
    body: &'a [u8],
}

fn parse_http_request(buffer: &[u8]) -> Option<HttpRequest<'_>> {
    let request = core::str::from_utf8(buffer).ok()?;
    let mut lines = request.lines();

    let first_line = lines.next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;

    // find body (after \r\n\r\n)
    let body_start = request
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(request.len());
    let body = &buffer[body_start..];

    Some(HttpRequest { method, path, body })
}

fn create_http_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    let body_bytes = body.as_bytes();
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body_bytes.len()
    );

    let mut response = Vec::with_capacity(header.len() + body_bytes.len());
    response.extend_from_slice(header.as_bytes());
    response.extend_from_slice(body_bytes);
    response
}

async fn handle_request(
    request: HttpRequest<'_>,
    signals: &Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) -> Vec<u8> {
    match (request.method, request.path) {
        ("GET", "/") => create_http_response("200 OK", "text/html", wifi_panel_str),
        ("GET", "/list") => {
            let scan_res = signals.wifi_scan_res.try_lock();
            let resp = match scan_res {
                Ok(ref resp) => resp.as_str(),
                Err(_) => "",
            };
            create_http_response("200 OK", "text/plain", resp)
        }
        ("POST", "/setup") => {
            let body_vec = request.body.to_vec();
            signals.wifi_conn_info_sig.signal(body_vec);
            create_http_response("200 OK", "text/plain", ".")
        }
        _ => create_http_response("404 Not Found", "text/plain", "Not Found"),
    }
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    _id: usize,
    stack: Stack<'static>,
    signals: Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) {
    let fut = async {
        let mut rx_buffer = [0; 1024];
        let mut tx_buffer = [0; 1024];
        let mut http_buffer = alloc::vec![0; HTTP_BUFFER_SIZE];

        loop {
            let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
            socket.set_timeout(Some(Duration::from_secs(10)));

            if socket.accept(80).await.is_err() {
                Timer::after(Duration::from_millis(100)).await;
                continue;
            }

            // read req
            let mut total_read = 0;
            loop {
                match socket.read(&mut http_buffer[total_read..]).await {
                    Ok(0) => break,
                    Ok(n) => {
                        total_read += n;
                        if http_buffer[..total_read]
                            .windows(4)
                            .any(|w| w == b"\r\n\r\n")
                        {
                            break;
                        }
                        if total_read >= HTTP_BUFFER_SIZE {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            if total_read == 0 {
                let _ = socket.close();
                continue;
            }

            // parse and handle request
            if let Some(req) = parse_http_request(&http_buffer[..total_read]) {
                let resp = handle_request(req, &signals, wifi_panel_str).await;
                let mut i = 0;

                while i < resp.len() {
                    match socket.write(&resp[i..]).await {
                        Ok(n) => {
                            i += n;
                        }
                        Err(e) => {
                            log::error!("Http wifimanager write error: {e:?}");
                            break;
                        }
                    }

                    _ = socket.flush().await;
                }
            }

            let _ = socket.close();
        }
    };

    embassy_futures::select::select(fut, signals.end_signalled()).await;
}

pub async fn run_http_server(
    spawner: &Spawner,
    ap_stack: Stack<'static>,
    signals: Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) {
    for id in 0..WEB_TASK_POOL_SIZE {
        spawner.must_spawn(web_task(id, ap_stack, signals.clone(), wifi_panel_str));
    }
}
