//! Optional TURN servers offered by the host: `GET ./ice-servers` (the Cloudflare Worker in
//! `worker/`). Static hosts answer 404 (and the dev server answers HTML); then the app keeps
//! STUN plus any `&turn=` from the room link. The request carries no room information: the
//! room ID and key live in the URL fragment, which is never sent.

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::window;

/// Entering a room waits at most this long for the host's answer.
const FETCH_TIMEOUT_MS: i32 = 3000;

/// `RTCIceServer` entries from the host, or `None` when it offers none.
pub async fn fetch_ice_servers() -> Option<js_sys::Array> {
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
