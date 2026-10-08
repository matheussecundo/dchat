const CACHE_NAME = "dchat-static-v4";
// Only files with fixed names: the CSS/JS/WASM have content hashes in their names and are
// cached when first fetched. Precaching a missing file would make the install fail.
// The manifest and icons are served cache-first: bump CACHE_NAME when they change.
const STATIC_ASSETS = [
  "./",
  "./index.html",
  "./manifest.webmanifest",
  "./icons/icon.svg",
  "./icons/icon-192.png",
  "./icons/icon-512.png",
  "./icons/icon-maskable-512.png",
  "./icons/apple-touch-icon.png",
];

// Invariant: This Service Worker ONLY caches immutable application shell assets.
// It NEVER stores user messages, room IDs, or encryption keys.

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      return cache.addAll(STATIC_ASSETS);
    })
  );
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) => {
      return Promise.all(
        keys.map((key) => {
          if (key !== CACHE_NAME) {
            return caches.delete(key);
          }
        })
      );
    })
  );
  self.clients.claim();
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Bypass WebSocket, non-GET requests and per-request TURN credentials entirely
  if (
    event.request.method !== "GET" ||
    url.pathname === "/ws" ||
    url.pathname === "/health" ||
    url.pathname.endsWith("/ice-servers")
  ) {
    return;
  }

  // Pages (index.html) go network-first so a new build reaches returning visitors; the
  // cached copy is only an offline fallback. Hashed .wasm/.js/.css stay cache-first.
  const isPage = event.request.mode === "navigate" || url.pathname === "/" || url.pathname.endsWith(".html");
  if (isPage) {
    event.respondWith(
      fetch(event.request)
        .then((networkResponse) => {
          if (networkResponse.status === 200) {
            const responseToCache = networkResponse.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(url.pathname, responseToCache));
          }
          return networkResponse;
        })
        .catch(() => caches.match(url.pathname).then((cached) => cached || caches.match("./")))
    );
    return;
  }

  event.respondWith(
    caches.match(event.request).then((cachedResponse) => {
      if (cachedResponse) {
        return cachedResponse;
      }
      return fetch(event.request).then((networkResponse) => {
        // Cache new static files (.wasm, .js, .css, images)
        if (
          networkResponse.status === 200 &&
          (url.pathname.endsWith(".wasm") ||
           url.pathname.endsWith(".js") ||
           url.pathname.endsWith(".css") ||
           url.pathname.endsWith(".html") ||
           url.pathname === "/")
        ) {
          const responseToCache = networkResponse.clone();
          caches.open(CACHE_NAME).then((cache) => {
            cache.put(event.request, responseToCache);
          });
        }
        return networkResponse;
      });
    })
  );
});
