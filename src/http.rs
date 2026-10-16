use crate::structs::{SetupData, WmInnerSignals};
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Stack};
use embassy_time::{Duration, Timer};

const WEB_TASK_POOL_SIZE: usize = 2;
const HTTP_BUFFER_SIZE: usize = 2048;
const HTTP_HEADER_BUFFER_SIZE: usize = 192;

struct HttpRequest<'a> {
    method: &'a str,
    path: &'a str,
    headers: &'a str,
    body: &'a [u8],
}

impl HttpRequest<'_> {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.split("\r\n").find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then_some(v.trim())
        })
    }

    fn content_length(&self) -> Option<usize> {
        self.header("Content-Length")?.parse().ok()
    }

    #[cfg(feature = "ota")]
    fn query(&self, key: &str) -> Option<&str> {
        let (_, query) = self.path.split_once('?')?;
        query.split('&').find_map(|kv| {
            kv.split_once('=')
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v)
        })
    }

    fn is(&self, method: &str, path: &str) -> bool {
        let route = self.path.split('?').next().unwrap_or_default();
        self.method.eq_ignore_ascii_case(method) && route == path
    }
}

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_http_request(buffer: &[u8]) -> Option<HttpRequest<'_>> {
    let header_end = header_end(buffer)?;
    let header_section = core::str::from_utf8(&buffer[..header_end]).ok()?;

    let (first_line, headers) = header_section
        .split_once("\r\n")
        .unwrap_or((header_section, ""));
    let mut parts = first_line.split_whitespace();

    Some(HttpRequest {
        method: parts.next()?,
        path: parts.next()?,
        headers,
        body: &buffer[header_end + 4..],
    })
}

/// Reads the head and, for requests other than the OTA upload (streamed later), the whole body.
/// Returns the number of bytes in `buf`, or `None` when the request doesn't fit or the peer stalls
/// (the socket timeout ends slow clients).
async fn read_request(socket: &mut TcpSocket<'_>, buf: &mut [u8]) -> Option<usize> {
    let mut total = 0;
    loop {
        if let Some(req) = parse_http_request(&buf[..total]) {
            if req.is("POST", "/update") {
                return Some(total);
            }

            let head_len = total - req.body.len();
            let needed = head_len.saturating_add(req.content_length().unwrap_or(0));
            if total >= needed {
                return Some(total);
            }
            if needed > buf.len() {
                return None;
            }
        } else if total == buf.len() {
            return None;
        }

        match socket.read(&mut buf[total..]).await {
            Ok(0) | Err(_) => return (total > 0).then_some(total),
            Ok(n) => total += n,
        }
    }
}

async fn write_response(
    socket: &mut TcpSocket<'_>,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> bool {
    let mut header: heapless::String<HTTP_HEADER_BUFFER_SIZE> = heapless::String::new();
    _ = core::fmt::write(
        &mut header,
        format_args!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ),
    );

    write_all(socket, header.as_bytes()).await && write_all(socket, body).await
}

async fn write_all(socket: &mut TcpSocket<'_>, data: &[u8]) -> bool {
    use embedded_io_async::Write;

    match socket.write_all(data).await {
        Ok(()) => socket.flush().await.is_ok(),
        Err(e) => {
            log::error!("Http wifimanager write error: {e:?}");
            false
        }
    }
}

#[cfg(feature = "ota")]
const UPDATE_PANEL_HTML: &str = include_minifier::include_minified!("src/update.html");
#[cfg(not(feature = "ota"))]
const UPDATE_PANEL_HTML: &str = "<html><body><p>OTA updates disabled</p></body></html>";

async fn handle_request(
    request: HttpRequest<'_>,
    signals: &WmInnerSignals,
    wifi_panel_str: &'static str,
    socket: &mut TcpSocket<'_>,
) {
    if request.is("GET", "/") {
        _ = write_response(socket, "200 OK", "text/html", wifi_panel_str.as_bytes()).await;
    } else if request.is("GET", "/update") {
        _ = write_response(socket, "200 OK", "text/html", UPDATE_PANEL_HTML.as_bytes()).await;
    } else if request.is("GET", "/list") {
        let scan_res = signals.wifi_scan_res.try_lock();
        let resp = scan_res.as_ref().map(|s| s.as_str()).unwrap_or_default();
        _ = write_response(socket, "200 OK", "text/plain", resp.as_bytes()).await;
    } else if request.is("POST", "/setup") {
        let Ok(setup) = SetupData::from_slice(request.body) else {
            _ = write_response(socket, "413 Payload Too Large", "text/plain", b"Too large").await;
            return;
        };

        signals.wifi_conn_info_sig.signal(setup);
        _ = write_response(socket, "200 OK", "text/plain", b".").await;
    } else if cfg!(feature = "ota") && request.is("POST", "/update") {
        #[cfg(feature = "ota")]
        handle_update_req(request, socket).await;
    } else {
        _ = write_response(socket, "404 Not Found", "text/plain", b"Not Found").await;
    }
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    stack: Stack<'static>,
    signals: &'static WmInnerSignals,
    wifi_panel_str: &'static str,
) {
    let fut = async {
        let mut rx_buffer = [0; 1024];
        let mut tx_buffer = [0; 1024];
        // heap, not task memory: only needed while the AP runs
        let mut http_buffer = alloc::vec![0; HTTP_BUFFER_SIZE];

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        loop {
            socket.set_timeout(Some(Duration::from_secs(10)));
            socket.set_nagle_enabled(false);
            if socket.accept(80).await.is_err() {
                Timer::after(Duration::from_millis(5)).await;
                continue;
            }

            match read_request(&mut socket, &mut http_buffer).await {
                Some(n) => match parse_http_request(&http_buffer[..n]) {
                    Some(req) => handle_request(req, signals, wifi_panel_str, &mut socket).await,
                    None => {
                        _ = write_response(&mut socket, "400 Bad Request", "text/plain", b"").await
                    }
                },
                None => {
                    _ = write_response(&mut socket, "413 Payload Too Large", "text/plain", b"")
                        .await
                }
            }

            socket.close();
            Timer::after_millis(5).await;
            socket.abort();
        }
    };

    embassy_futures::select::select(fut, signals.end_signalled()).await;
}

/// Streams the firmware into the OTA partition. The response is sent before the upload ends,
/// as `PROGRESS:<received>,<total>` lines and a final `DONE:` or `ERROR:` line.
#[cfg(feature = "ota")]
async fn handle_update_req(req: HttpRequest<'_>, socket: &mut TcpSocket<'_>) {
    let params = (|| {
        let size: u32 = req.query("size")?.trim().parse().ok()?;
        let crc: u32 = req.query("crc")?.trim().parse().ok()?;
        Some((size, crc, req.content_length()?))
    })();
    let Some((size, crc, content_length)) = params else {
        _ = write_response(
            socket,
            "400 Bad Request",
            "text/plain",
            b"ERROR:bad request\n",
        )
        .await;
        return;
    };

    log::info!("Start ota update. Size: {size} crc: {crc}");
    // SAFETY: flash is only used here while the wifimanager AP runs
    let ota = esp_hal_ota::Ota::new(esp_storage::FlashStorage::new(unsafe {
        esp_hal::peripherals::FLASH::steal()
    }));
    let Ok(mut ota) = ota else {
        _ = write_response(
            socket,
            "500 Internal Server Error",
            "text/plain",
            b"ERROR:ota init\n",
        )
        .await;
        return;
    };
    if let Err(e) = ota.begin(size, crc) {
        log::warn!("Ota begin error: {e:?}");
        _ = write_response(
            socket,
            "500 Internal Server Error",
            "text/plain",
            b"ERROR:ota begin\n",
        )
        .await;
        return;
    }

    // close-delimited body, so progress can be streamed
    if !write_all(
        socket,
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n",
    )
    .await
    {
        return;
    }

    let mut ota_buffer = alloc::vec![0u8; 4096];
    let first = req.body.len().min(ota_buffer.len());
    ota_buffer[..first].copy_from_slice(&req.body[..first]);
    let mut buffer_pos = first;
    let mut total = 0;

    let result: &[u8] = loop {
        let n = if buffer_pos == ota_buffer.len() || total + buffer_pos >= content_length {
            0
        } else {
            match socket.read(&mut ota_buffer[buffer_pos..]).await {
                Ok(0) | Err(_) => break b"ERROR:connection lost\n",
                Ok(n) => n,
            }
        };
        buffer_pos += n;

        if buffer_pos < ota_buffer.len() && total + buffer_pos < content_length {
            continue;
        }

        total += buffer_pos;
        let mut progress: heapless::String<48> = heapless::String::new();
        _ = core::fmt::write(
            &mut progress,
            format_args!("PROGRESS:{total},{content_length}\n"),
        );
        _ = write_all(socket, progress.as_bytes()).await;

        match ota.write(&ota_buffer[..buffer_pos]) {
            Ok(true) => {
                if ota.finish(true).is_ok() {
                    log::info!("OTA restart!");
                    _ = write_all(socket, b"DONE:OTA Update Successful. Restarting...\n").await;
                    Timer::after(Duration::from_millis(100)).await;
                    esp_hal::system::software_reset();
                }

                log::error!("OTA flash verify failed!");
                break b"ERROR:verify failed\n";
            }
            Ok(false) => {}
            Err(e) => {
                log::error!("OTA write error: {e:?}");
                break b"ERROR:write failed\n";
            }
        }

        buffer_pos = 0;
        if total >= content_length {
            break b"ERROR:upload ended before image was complete\n";
        }
    };

    _ = write_all(socket, result).await;
}

pub async fn run_http_server(
    spawner: &Spawner,
    ap_stack: Stack<'static>,
    signals: &'static WmInnerSignals,
    wifi_panel_str: &'static str,
) -> crate::Result<()> {
    for _ in 0..WEB_TASK_POOL_SIZE {
        spawner.spawn(web_task(ap_stack, signals, wifi_panel_str)?);
    }

    Ok(())
}
