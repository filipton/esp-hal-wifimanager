use crate::structs::WmInnerSignals;
use alloc::{format, rc::Rc, vec::Vec};
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Stack};
use embassy_time::{Duration, Timer};
use embedded_io_async::Write;

const WEB_TASK_POOL_SIZE: usize = 2;
const HTTP_BUFFER_SIZE: usize = 2048;

struct HttpRequest<'a> {
    method: &'a str,
    path: &'a str,
    headers: &'a [u8],
    body: &'a [u8],
}

fn parse_http_request(buffer: &[u8]) -> Option<HttpRequest<'_>> {
    let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;

    let header_section = core::str::from_utf8(&buffer[..header_end]).ok()?;

    let mut lines = header_section.lines();
    let first_line = lines.next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;

    let headers_start = header_section.find("\r\n").map(|i| i + 2).unwrap_or(0);
    let headers = &buffer[headers_start..header_end];

    let body = &buffer[header_end + 4..];

    Some(HttpRequest {
        method,
        path,
        headers,
        body,
    })
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

#[cfg(feature = "ota")]
const UPDATE_PANEL_HTML: &str = include_str!("update.html");
#[cfg(not(feature = "ota"))]
const UPDATE_PANEL_HTML: &str = "<html><body><p>OTA updates disabled</p></body></html>";

async fn handle_request(
    request: HttpRequest<'_>,
    signals: &Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) -> Vec<u8> {
    match (request.method, request.path) {
        ("GET", "/") => create_http_response("200 OK", "text/html", wifi_panel_str),
        ("GET", "/update") => create_http_response("200 OK", "text/html", UPDATE_PANEL_HTML),
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

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(10)));
        socket.set_nagle_enabled(false);
        loop {
            if socket.accept(80).await.is_err() {
                Timer::after(Duration::from_millis(5)).await;
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
                if req.path.starts_with("/update") && req.method.to_uppercase() == "POST" {
                    #[cfg(feature = "ota")]
                    if handle_update_req(req, &mut socket).await.is_none() {
                        let resp = create_http_response(
                            "500 Internal Server Error",
                            "text/plain",
                            "Update handler failed",
                        );
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
                } else {
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
            }

            Timer::after_millis(5).await;
            let _ = socket.close();
            Timer::after_millis(5).await;
            socket.abort();
        }
    };

    embassy_futures::select::select(fut, signals.end_signalled()).await;
}

#[cfg(feature = "ota")]
async fn handle_update_req(req: HttpRequest<'_>, socket: &mut TcpSocket<'_>) -> Option<()> {
    let Some(query) = req.path.split("?").nth(1) else {
        return None;
    };

    let mut query = query.split("&").map(|q| {
        let mut split = q.split("=");
        (
            split.next().unwrap_or_default(),
            split.next().unwrap_or_default(),
        )
    });

    let size: u32 = query.find(|(k, _)| *k == "size")?.1.trim().parse().ok()?;
    let crc: u32 = query.find(|(k, _)| *k == "crc")?.1.trim().parse().ok()?;

    log::info!("Start ota update. Size: {size} crc: {crc}");
    let headers = core::str::from_utf8(req.headers).ok()?;
    let content_length: usize = headers
        .split("\r\n")
        .map(|h| {
            let mut split = h.splitn(2, ": ");
            let k = split.next().unwrap_or_default();
            let v = split.next().unwrap_or_default();
            (k, v)
        })
        .find(|(k, _)| k.to_uppercase() == "CONTENT-LENGTH")?
        .1
        .trim()
        .parse()
        .ok()?;

    let mut ota = esp_hal_ota::Ota::new(esp_storage::FlashStorage::new(unsafe {
        esp_hal::peripherals::FLASH::steal()
    }))
    .ok()?;
    ota.ota_begin(size, crc).ok()?;

    let mut ota_buffer = [0; 4096];
    ota_buffer[..req.body.len()].copy_from_slice(&req.body);
    let mut buffer_pos = req.body.len();
    let mut total = 0;

    loop {
        match socket.read(&mut ota_buffer[buffer_pos..]).await {
            Ok(0) => {
                if buffer_pos > 0 {
                    total += buffer_pos;
                    log::info!("read body: {} (total: {}) - final chunk", buffer_pos, total);
                    let res = ota.ota_write_chunk(&ota_buffer[..buffer_pos]);
                    if res == Ok(true) {
                        if ota.ota_flush(true, true).is_ok() {
                            log::info!("OTA restart!");
                            let resp = create_http_response(
                                "200 OK",
                                "text/plain",
                                "OTA Update Successful. Restarting...",
                            );
                            socket.write_all(&resp).await.ok()?;
                            Timer::after(Duration::from_millis(100)).await;
                            esp_hal::system::software_reset();
                        } else {
                            log::error!("OTA flash verify failed!");
                        }
                    }
                }
                break;
            }
            Ok(n) => {
                buffer_pos += n;

                if buffer_pos == 4096 || total + buffer_pos >= content_length {
                    total += buffer_pos;
                    log::info!("read body: {} (total: {})", buffer_pos, total);

                    let progress_msg = format!("PROGRESS:{},{}\n", total, content_length);
                    _ = socket.write_all(progress_msg.as_bytes()).await;
                    _ = socket.flush().await;

                    let res = ota.ota_write_chunk(&ota_buffer[..buffer_pos]);
                    if res == Ok(true) {
                        if ota.ota_flush(true, true).is_ok() {
                            log::info!("OTA restart!");
                            let final_msg = "DONE:OTA Update Successful. Restarting...\n";
                            _ = socket.write_all(final_msg.as_bytes()).await;
                            _ = socket.flush().await;

                            Timer::after(Duration::from_millis(100)).await;
                            esp_hal::system::software_reset();
                        } else {
                            log::error!("OTA flash verify failed!");
                        }
                    }
                    buffer_pos = 0;

                    if total >= content_length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }

    let resp = create_http_response("200 OK", "text/html", "Uploaded");
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

    Some(())
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
