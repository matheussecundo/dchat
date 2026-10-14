//! Optional TURN servers offered by the host: `GET ./ice-servers` (the Cloudflare Worker in
//! `worker/`). Static hosts answer 404 (and the dev server answers HTML); then the app keeps
//! STUN plus any `&turn=` from the room link. The request carries no room information: the
//! room ID and key live in the URL fragment, which is never sent.

use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::window;

thread_local! {
    /// The page's one request to the host (`host_ice_servers`), shared by every caller.
    static REQUEST: RefCell<Option<js_sys::Promise>> = const { RefCell::new(None) };
}

/// The host's ICE servers, asked once per page load: the join screen asks early (to offer
/// "hide my IP" when the host has TURN), entering the room reuses the answer.
pub async fn host_ice_servers() -> Option<js_sys::Array> {
    let request = REQUEST.with(|r| {
        r.borrow_mut()
            .get_or_insert_with(|| {
                wasm_bindgen_futures::future_to_promise(async {
                    Ok(fetch_ice_servers().await.map(JsValue::from).unwrap_or(JsValue::NULL))
                })
            })
            .clone()
    });
    JsFuture::from(request).await.ok()?.dyn_into().ok()
}

/// Whether `servers` include a TURN server.
pub fn offers_turn(servers: &js_sys::Array) -> bool {
    servers.iter().any(|server| {
        let urls = js_sys::Reflect::get(&server, &"urls".into()).unwrap_or(JsValue::UNDEFINED);
        let list: Vec<String> = match urls.dyn_ref::<js_sys::Array>() {
            Some(list) => list.iter().filter_map(|u| u.as_string()).collect(),
            None => urls.as_string().into_iter().collect(),
        };
        list.iter().any(|u| u.starts_with("turn:") || u.starts_with("turns:"))
    })
}

/// Entering a room waits at most this long for the host's answer.
const FETCH_TIMEOUT_MS: i32 = 3000;

/// `RTCIceServer` entries from the host, or `None` when it offers none.
async fn fetch_ice_servers() -> Option<js_sys::Array> {
    let win = window()?;
    let request = win.fetch_with_str("./ice-servers");
    let timeout = js_sys::Promise::new(&mut |resolve, _| {
        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, FETCH_TIMEOUT_MS);
    });
    let first = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of2(&request, &timeout)))
        .await
        .ok()?;
    // The timeout resolves with `undefined`, which is not a Response.
    let response: web_sys::Response = first.dyn_into().ok()?;
    if !response.ok() {
        return None;
    }
    let body = JsFuture::from(response.json().ok()?).await.ok()?;
    let servers: js_sys::Array = js_sys::Reflect::get(&body, &"iceServers".into()).ok()?.dyn_into().ok()?;
    let valid: js_sys::Array = servers.iter().filter(is_ice_server).collect();
    (valid.length() > 0).then_some(valid)
}

/// An object whose `urls` is a string or an array of strings.
fn is_ice_server(server: &JsValue) -> bool {
    let Ok(urls) = js_sys::Reflect::get(server, &"urls".into()) else {
        return false;
    };
    urls.is_string()
        || urls
            .dyn_ref::<js_sys::Array>()
            .is_some_and(|list| list.length() > 0 && list.iter().all(|u| u.is_string()))
}
