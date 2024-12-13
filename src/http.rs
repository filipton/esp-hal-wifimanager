use crate::structs::WmInnerSignals;
use alloc::rc::Rc;
use embassy_net::{tcp::TcpSocket, IpListenEndpoint, Stack};
use embedded_io_async::Write;
use httparse::Header;

#[embassy_executor::task]
pub async fn run_http_server(
    ap_stack: Stack<'static>,
    signals: Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) {
    let fut = async {
        let mut rx_buffer = [0; 4096];
        let mut tx_buffer = [0; 4096];

        let mut socket = TcpSocket::new(ap_stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(60)));

        let mut buf = [0; 2048];
        loop {
            if let Err(e) = socket
                .accept(IpListenEndpoint {
                    addr: None,
                    port: 80,
                })
                .await
            {
                log::error!("socket.accept error: {e:?}");
            }

            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => {
                        log::warn!("socket.read EOF");
                        break;
                    }
                    Ok(n) => {
                        let mut headers = [httparse::EMPTY_HEADER; 32];
                        let mut req = httparse::Request::new(&mut headers);

                        let body_offset = match req.parse(&buf[..n]) {
                            Ok(res) => {
                                if res.is_partial() {
                                    log::error!("request is partial");
                                    break;
                                }

                                res.unwrap()
                            }
                            Err(e) => {
                                log::error!("request.parse error: {e:?}");
                                break;
                            }
                        };

                        let (path, method) = (req.path.unwrap_or("/"), req.method.unwrap_or("GET"));
                        match (path, method) {
                            ("/", "GET") => {
                                let resp_len = alloc::format!("{}", wifi_panel_str.len());
                                let http_resp = construct_http_resp(
                                    200,
                                    "OK",
                                    &[
                                        Header {
                                            name: "Content-Type",
                                            value: b"text/html",
                                        },
                                        Header {
                                            name: "Content-Length",
                                            value: resp_len.as_bytes(),
                                        },
                                    ],
                                    wifi_panel_str.as_bytes(),
                                );

                                let res = socket.write_all(&http_resp).await;
                                if let Err(e) = res {
                                    log::error!("socket.write_all err: {e:?}");
                                    break;
                                }

                                _ = socket.flush().await;
                            }
                            ("/setup", "POST") => {
                                signals
                                    .wifi_conn_info_sig
                                    .signal(buf[body_offset..n].to_vec());
                                let wifi_connected = signals.wifi_conn_res_sig.wait().await;
                                let resp = alloc::format!("{}", wifi_connected);
                                let resp_len = alloc::format!("{}", resp.len());

                                let http_resp = construct_http_resp(
                                    200,
                                    "OK",
                                    &[Header {
                                        name: "Content-Length",
                                        value: resp_len.as_bytes(),
                                    }],
                                    resp.as_bytes(),
                                );

                                let res = socket.write_all(&http_resp).await;
                                if let Err(e) = res {
                                    log::error!("socket.write_all err: {e:?}");
                                    break;
                                }

                                _ = socket.flush().await;
                            }
                            ("/list", "GET") => {
                                let resp_res = signals.wifi_scan_res.try_lock();
                                let resp = match resp_res {
                                    Ok(ref resp) => resp.as_str(),
                                    Err(_) => "",
                                };

                                let resp_len = alloc::format!("{}", resp.len());

                                let http_resp = construct_http_resp(
                                    200,
                                    "OK",
                                    &[Header {
                                        name: "Content-Length",
                                        value: resp_len.as_bytes(),
                                    }],
                                    resp.as_bytes(),
                                );

                                let res = socket.write_all(&http_resp).await;
                                if let Err(e) = res {
                                    log::error!("socket.write_all err: {e:?}");
                                    break;
                                }

                                _ = socket.flush().await;
                            }
                            _ => {
                                log::warn!("NOT FOUND: {req:?}");
                                let res = socket
                                    .write_all(&construct_http_resp(
                                        404,
                                        "Not Found",
                                        &[Header {
                                            name: "Content-Length",
                                            value: b"0",
                                        }],
                                        &[],
                                    ))
                                    .await;

                                if let Err(e) = res {
                                    log::error!("socket.write_all err: {e:?}");
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("socket.read error: {e:?}");
                        break;
                    }
                }
            }

            _ = socket.close();
            _ = socket.abort();
        }
    };

    embassy_futures::select::select(fut, signals.end_signalled()).await;
}

pub fn construct_http_resp(
    status_code: u16,
    status_text: &str,
    headers: &[Header],
    body: &[u8],
) -> alloc::vec::Vec<u8> {
    let mut buf = alloc::vec::Vec::new();
    buf.extend_from_slice(alloc::format!("HTTP/1.1 {status_code} {status_text}\r\n").as_bytes());
    for header in headers {
        buf.extend_from_slice(
            alloc::format!(
                "{}: {}\r\n",
                header.name,
                core::str::from_utf8(header.value).unwrap()
            )
            .as_bytes(),
        );
    }
    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(body);
    buf
}
