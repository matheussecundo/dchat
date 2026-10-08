//! The installed app (PWA): the install offer, the app icon's badge, and room links handed
//! to an app window that is already open (`launchQueue`). Everything is feature-detected:
//! browsers without these APIs show no Install button and no badge, and open links in tabs.
//! Nothing is remembered: a dismissed offer or hint comes back on the next load.

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::window;

/// Where index.html keeps Chromium's `beforeinstallprompt` event (it can fire before the
/// wasm has loaded), and the window event it raises when that changes.
const INSTALL_PROMPT: &str = "dchatInstallPrompt";
const INSTALL_CHANGED: &str = "dchat-installable";

fn get(target: &JsValue, name: &str) -> JsValue {
    js_sys::Reflect::get(target, &name.into()).unwrap_or(JsValue::UNDEFINED)
}

/// `target[name](...args)` when that is a function.
fn call(target: &JsValue, name: &str, args: &js_sys::Array) -> Option<JsValue> {
    let function: js_sys::Function = get(target, name).dyn_into().ok()?;
    function.apply(target, args).ok()
}

/// Let a returned promise settle without an "unhandled rejection" in the console.
fn settle_quietly(result: Option<JsValue>) {
    if let Some(promise) = result.and_then(|v| v.dyn_into::<js_sys::Promise>().ok()) {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}

/// Whether the browser offered installing the app, so the Install button can open its dialog.
pub fn install_available() -> bool {
    window().is_some_and(|w| get(&w, INSTALL_PROMPT).is_object())
}

/// Run `on_change` whenever the install offer appears or goes away.
pub fn on_install_change(on_change: impl Fn() + 'static) {
    let Some(win) = window() else {
        return;
    };
    let cb = Closure::<dyn Fn()>::new(on_change);
    let _ = win.add_event_listener_with_callback(INSTALL_CHANGED, cb.as_ref().unchecked_ref());
    cb.forget();
}

/// Open the browser's install dialog. Call it straight from a click (it needs the user
/// gesture). An offer can be used once, so it is dropped either way.
pub fn prompt_install() {
    let Some(win) = window() else {
        return;
    };
    let offer = get(&win, INSTALL_PROMPT);
    settle_quietly(call(&offer, "prompt", &js_sys::Array::new()));
    let _ = js_sys::Reflect::set(&win, &INSTALL_PROMPT.into(), &JsValue::NULL);
    if let Ok(event) = web_sys::Event::new(INSTALL_CHANGED) {
        let _ = win.dispatch_event(&event);
    }
}

/// Whether this page runs as the installed app (its own window, or from the iOS home screen).
pub fn is_installed() -> bool {
    let Some(win) = window() else {
        return false;
    };
    let standalone = win
        .match_media("(display-mode: standalone)")
        .ok()
        .flatten()
        .is_some_and(|query| query.matches());
    standalone || get(&win.navigator(), "standalone").as_bool() == Some(true)
}

/// Whether to show how to install from Apple's menus: there the page has no install offer
/// to open.
pub fn apple_install_hint() -> bool {
    let Some(win) = window() else {
        return false;
    };
    let nav = win.navigator();
    !is_installed() && !install_available() && apple_webkit(&nav.user_agent().unwrap_or_default(), nav.max_touch_points())
}

/// An iPhone or iPad (every browser there is WebKit and installs from Share → Add to Home
/// Screen; iPadOS says it is a Mac with touch), or Safari on a Mac (File → Add to Dock).
fn apple_webkit(user_agent: &str, touch_points: i32) -> bool {
    let mac = user_agent.contains("Macintosh");
    let ios = ["iPhone", "iPad", "iPod"].iter().any(|d| user_agent.contains(d)) || (mac && touch_points > 1);
    let other_engine = ["Chrome/", "Chromium/", "Edg/", "OPR/", "Firefox/"].iter().any(|b| user_agent.contains(b));
    ios || (mac && user_agent.contains("Safari/") && !other_engine)
}

/// Show `count` on the installed app's icon, or no badge at 0. Ignored where unsupported or
/// refused (iOS wants the notification permission, which dchat never asks for).
pub fn set_badge(count: usize) {
    let Some(win) = window() else {
        return;
    };
    let nav: JsValue = win.navigator().into();
    let result = if count > 0 {
        call(&nav, "setAppBadge", &js_sys::Array::of1(&(count as f64).into()))
    } else {
        call(&nav, "clearAppBadge", &js_sys::Array::new())
    };
    settle_quietly(result);
}

/// Hand every link the installed app is launched with to `on_link`. Where the app has a
/// single window (Android), a room link reaches the open window here instead of loading
/// over it; a new window gets the link it was opened with.
pub fn on_launch(on_link: impl Fn(String) + 'static) {
    let Some(win) = window() else {
        return;
    };
    let queue = get(&win, "launchQueue");
    if !queue.is_object() {
        return;
    }
    let consumer = Closure::<dyn Fn(JsValue)>::new(move |params: JsValue| {
        if let Some(url) = get(&params, "targetURL").as_string() {
            on_link(url);
        }
    });
    let _ = call(&queue, "setConsumer", &js_sys::Array::of1(consumer.as_ref()));
    consumer.forget();
}

#[cfg(test)]
mod tests {
    use super::apple_webkit;

    #[test]
    fn apple_hint_only_where_there_is_no_install_offer() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
        let iphone_chrome = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/130.0 Mobile/15E148 Safari/604.1";
        let mac_safari = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
        let mac_chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
        let mac_firefox = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:131.0) Gecko/20100101 Firefox/131.0";
        let android = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36";
        assert!(apple_webkit(iphone, 5));
        assert!(apple_webkit(iphone_chrome, 5));
        assert!(apple_webkit(mac_safari, 0));
        // iPadOS reports a Mac; only touch tells it apart.
        assert!(apple_webkit(mac_chrome, 5));
        assert!(!apple_webkit(mac_chrome, 0));
        assert!(!apple_webkit(mac_firefox, 0));
        assert!(!apple_webkit(android, 5));
    }
}
