use crate::structs::{AutoSetupSettings, WmInnerSignals};
use alloc::rc::Rc;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::Duration;
use picoserve::{
    extract::{Json, State},
    routing::{get, get_service, post},
    AppRouter, AppWithStateBuilder,
};

const WEB_TASK_POOL_SIZE: usize = 2;

#[derive(Clone)]
struct AppState {
    signals: Rc<WmInnerSignals>,
}

struct AppProps {
    wifi_panel_str: &'static str,
}

impl AppWithStateBuilder for AppProps {
    type State = AppState;
    type PathRouter = impl picoserve::routing::PathRouter<AppState>;

    fn build_app(self) -> picoserve::Router<Self::PathRouter, Self::State> {
        picoserve::Router::new()
            .route(
                "/",
                get_service(picoserve::response::File::html(self.wifi_panel_str)),
            )
            .route(
                "/list",
                get(|State(app_state): State<AppState>| async move {
                    let resp_res = app_state.signals.wifi_scan_res.try_lock();
                    let resp = match resp_res {
                        Ok(ref resp) => resp.as_str(),
                        Err(_) => "",
                    };

                    alloc::string::ToString::to_string(&resp)
                }),
            )
            .route(
                "/setup",
                post(|State(app_state): State<AppState>, Json(setup): Json<AutoSetupSettings>| async move {
                    app_state.signals
                        .wifi_conn_info_sig
                        .signal(serde_json::to_vec(&setup).expect("Shouldnt error?"));

                    let wifi_connected = app_state.signals.wifi_conn_res_sig.wait().await;
                    alloc::format!("{}", wifi_connected)
                }),
            )
    }
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
    config: &'static picoserve::Config<Duration>,
    signals: Rc<WmInnerSignals>,
) {
    let port = 80;
    let mut tcp_rx_buffer = alloc::vec![0; 1024];
    let mut tcp_tx_buffer = alloc::vec![0; 1024];
    let mut http_buffer = alloc::vec![0; 2048];

    let state = AppState {
        signals: signals.clone(),
    };

    let fut = picoserve::listen_and_serve_with_state(
        id,
        app,
        config,
        stack,
        port,
        &mut tcp_rx_buffer,
        &mut tcp_tx_buffer,
        &mut http_buffer,
        &state,
    );

    embassy_futures::select::select(fut, signals.end_signalled()).await;
}

pub async fn run_http_server(
    spawner: &Spawner,
    ap_stack: Stack<'static>,
    signals: Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) {
    let app = AppProps { wifi_panel_str };
    let app = picoserve::make_static!(AppRouter<AppProps>, app.build_app());

    let config = picoserve::make_static!(
        picoserve::Config<Duration>,
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Some(Duration::from_secs(1)),
            read_request: Some(Duration::from_secs(1)),
            write: Some(Duration::from_secs(1)),
        })
        .keep_connection_alive()
    );

    for id in 0..WEB_TASK_POOL_SIZE {
        spawner.must_spawn(web_task(id, ap_stack, app, config, signals.clone()));
    }
}
