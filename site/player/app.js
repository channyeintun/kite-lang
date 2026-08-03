
const HOSTS = {
    "audio": {
      note: (...args) => missing("audio", "note")(...args),
      silence: (...args) => missing("audio", "silence")(...args),
    },
};

if (HOSTS.net) {
  const REQUESTS = [];
  const STREAMS = [];
  const parseHeaders = (text) => {
    const out = {};
    for (const line of String(text).split("\n")) {
      const at = line.indexOf(":");
      if (at > 0) out[line.slice(0, at).trim()] = line.slice(at + 1).trim();
    }
    return out;
  };
  HOSTS.net = {
    fetch_start: (method, url, body, headers) => {
      const request = { state: 0, status: 0, body: "", error: "", headers: null };
      const id = REQUESTS.push(request) - 1;
      const init = { method: S(method), headers: parseHeaders(S(headers)) };
      if (S(body) !== "") init.body = S(body);
      fetch(S(url), init)
        .then(async (response) => {
          request.status = response.status;
          request.headers = response.headers;
          request.body = await response.text();
          request.state = 1;
        })
        .catch((e) => {
          request.error = String(e && e.message ? e.message : e);
          request.state = 2;
        });
      return BigInt(id);
    },
    fetch_state: (id) => BigInt(REQUESTS[Number(id)].state),
    fetch_status: (id) => BigInt(REQUESTS[Number(id)].status),
    fetch_body: (id) => intern(REQUESTS[Number(id)].body),
    fetch_header: (id, name) => {
      const headers = REQUESTS[Number(id)].headers;
      return intern(headers ? headers.get(S(name)) ?? "" : "");
    },
    fetch_error: (id) => intern(REQUESTS[Number(id)].error),

    sse_open: (url, names) => {
      const s = { state: 0, queue: [], taken: { name: "", id: "" }, source: null };
      const id = STREAMS.push(s) - 1;
      if (typeof EventSource === "undefined") {
        s.state = 2;
        return BigInt(id);
      }
      const source = new EventSource(S(url));
      s.source = source;
      s.take = (e) => {
        s.queue.push({ name: e.type || "message", id: e.lastEventId || "", data: e.data ?? "" });
      };
      source.onopen = () => { s.state = 1; };
      source.onmessage = s.take;
      for (const name of S(names).split("\n")) {
        if (name !== "") source.addEventListener(name, s.take);
      }
      source.onerror = () => { if (source.readyState === 2) s.state = 2; };
      return BigInt(id);
    },
    sse_state: (id) => BigInt(STREAMS[Number(id)].state),
    sse_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    sse_next: (id) => {
      const s = STREAMS[Number(id)];
      const e = s.queue.shift();
      if (e === undefined) {
        s.taken = { name: "", id: "" };
        return intern("");
      }
      s.taken = { name: e.name, id: e.id };
      return intern(e.data);
    },
    sse_event_name: (id) => intern(STREAMS[Number(id)].taken.name),
    sse_event_id: (id) => intern(STREAMS[Number(id)].taken.id),
    sse_listen: (id, name) => {
      const s = STREAMS[Number(id)];
      if (s.source) s.source.addEventListener(S(name), s.take);
      return 1n;
    },
    sse_close: (id) => {
      const s = STREAMS[Number(id)];
      if (s.source) s.source.close();
      s.state = 3;
      return 1n;
    },

    socket_open: (url) => {
      const s = { state: 0, queue: [], error: "", socket: null };
      const id = STREAMS.push(s) - 1;
      if (typeof WebSocket === "undefined") {
        s.state = 2;
        s.error = "this host has no WebSocket";
        return BigInt(id);
      }
      let socket;
      try {
        socket = new WebSocket(S(url));
      } catch (e) {
        s.state = 2;
        s.error = String(e && e.message ? e.message : e);
        return BigInt(id);
      }
      s.socket = socket;
      socket.onopen = () => { s.state = 1; };
      socket.onmessage = (e) => { if (typeof e.data === "string") s.queue.push(e.data); };
      socket.onerror = () => {
        if (s.state !== 3) {
          s.state = 2;
          if (s.error === "") s.error = "the connection failed";
        }
      };
      socket.onclose = () => { if (s.state !== 2) s.state = 3; };
      return BigInt(id);
    },
    socket_state: (id) => BigInt(STREAMS[Number(id)].state),
    socket_pending: (id) => BigInt(STREAMS[Number(id)].queue.length),
    socket_next: (id) => {
      const s = STREAMS[Number(id)];
      const message = s.queue.shift();
      return intern(message === undefined ? "" : message);
    },
    socket_send: (id, message) => {
      const s = STREAMS[Number(id)];
      if (s.state !== 1 || !s.socket) return 0n;
      s.socket.send(S(message));
      return 1n;
    },
    socket_error: (id) => intern(STREAMS[Number(id)].error),
    socket_close: (id) => {
      const s = STREAMS[Number(id)];
      if (s.socket) s.socket.close();
      s.state = 3;
      return 1n;
    },
  };
}

if (HOSTS.crypto) {
  const WORK = [];
  const KEYS = [];
  const subtle = globalThis.crypto && globalThis.crypto.subtle;
  const hex = (buffer) =>
    [...new Uint8Array(buffer)].map((b) => b.toString(16).padStart(2, "0")).join("");
  const unhex = (text) =>
    new Uint8Array((String(text).match(/../g) ?? []).map((b) => parseInt(b, 16)));
  const start = (promise) => {
    const work = { state: 0, result: "", error: "" };
    const id = WORK.push(work) - 1;
    promise
      .then((value) => {
        work.result = value;
        work.state = 1;
      })
      .catch((e) => {
        work.error = String(e && e.message ? e.message : e);
        work.state = 2;
      });
    return BigInt(id);
  };
  const bytes = (text) => new TextEncoder().encode(text);
  const keep = (key) => String(KEYS.push(key) - 1);
  const unsupported = (name) => (e) => {
    if (e && e.name === "NotSupportedError") {
      throw new Error(`this host's WebCrypto has no ${name} — Node 24 and current browsers do`);
    }
    throw e;
  };
  HOSTS.crypto = {
    random_hex: (count) => {
      const out = new Uint8Array(Number(count));
      globalThis.crypto.getRandomValues(out);
      return intern(hex(out.buffer));
    },
    digest_start: (algorithm, text) =>
      start(subtle.digest(S(algorithm), bytes(S(text))).then(hex)),
    hmac_start: (algorithm, key, text) =>
      start(
        subtle
          .importKey("raw", bytes(S(key)), { name: "HMAC", hash: S(algorithm) }, false, [
            "sign",
          ])
          .then((k) => subtle.sign("HMAC", k, bytes(S(text))))
          .then(hex),
      ),
    derive_start: (password, salt, iterations) =>
      start(
        subtle
          .importKey("raw", bytes(S(password)), "PBKDF2", false, ["deriveBits"])
          .then((k) =>
            subtle.deriveBits(
              {
                name: "PBKDF2",
                salt: unhex(S(salt)),
                iterations: Number(iterations),
                hash: "SHA-256",
              },
              k,
              256,
            ),
          )
          .then(hex),
      ),
    key_generate_start: (kind) => {
      const name = S(kind);
      if (name === "AES-GCM") {
        return start(
          subtle.generateKey({ name, length: 256 }, false, ["encrypt", "decrypt"]).then(keep),
        );
      }
      const usages = name === "Ed25519" ? ["sign", "verify"] : ["deriveBits"];
      return start(subtle.generateKey(name, false, usages).catch(unsupported(name)).then(keep));
    },
    key_import_start: (material) => {
      const text = S(material);
      if (!/^[0-9a-f]{64}$/i.test(text)) {
        return start(Promise.reject(new Error("a key is 32 bytes — 64 hex characters")));
      }
      return start(
        subtle.importKey("raw", unhex(text), "AES-GCM", false, ["encrypt", "decrypt"]).then(keep),
      );
    },
    key_public_start: (key) =>
      start(
        Promise.resolve(KEYS[Number(key)])
          .then((pair) => subtle.exportKey("raw", pair.publicKey))
          .then(hex),
      ),
    seal_start: (key, nonce, plaintext) =>
      start(
        subtle
          .encrypt(
            { name: "AES-GCM", iv: unhex(S(nonce)) },
            KEYS[Number(key)],
            bytes(S(plaintext)),
          )
          .then(hex),
      ),
    open_start: (key, nonce, cipher) =>
      start(
        subtle
          .decrypt(
            { name: "AES-GCM", iv: unhex(S(nonce)) },
            KEYS[Number(key)],
            unhex(S(cipher)),
          )
          .then((clear) => new TextDecoder().decode(clear))
          .catch(() => {
            throw new Error("the sealed text was altered, or sealed under a different key");
          }),
      ),
    sign_start: (key, text) =>
      start(subtle.sign("Ed25519", KEYS[Number(key)].privateKey, bytes(S(text))).then(hex)),
    verify_start: (pub, text, signature) =>
      start(
        subtle
          .importKey("raw", unhex(S(pub)), "Ed25519", false, ["verify"])
          .catch(unsupported("Ed25519"))
          .then((k) => subtle.verify("Ed25519", k, unhex(S(signature)), bytes(S(text))))
          .then((ok) => (ok ? "true" : "false")),
      ),
    agree_start: (key, pub) =>
      start(
        subtle
          .importKey("raw", unhex(S(pub)), "X25519", false, [])
          .catch(unsupported("X25519"))
          .then((theirs) =>
            subtle.deriveBits({ name: "X25519", public: theirs }, KEYS[Number(key)].privateKey, 256),
          )
          .then((shared) => subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]))
          .then((k) =>
            subtle.deriveKey(
              { name: "HKDF", hash: "SHA-256", salt: new Uint8Array(0), info: bytes("kite crypto.agree v1") },
              k,
              { name: "AES-GCM", length: 256 },
              false,
              ["encrypt", "decrypt"],
            ),
          )
          .then(keep),
      ),
    work_state: (id) => BigInt(WORK[Number(id)].state),
    work_result: (id) => intern(WORK[Number(id)].result),
    work_error: (id) => intern(WORK[Number(id)].error),
    constant_time_equal: (a, b) => {
      const x = S(a);
      const y = S(b);
      let diff = x.length ^ y.length;
      for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
        diff |= (x.charCodeAt(i % x.length) || 0) ^ (y.charCodeAt(i % y.length) || 0);
      }
      return diff === 0 ? 1 : 0;
    },
  };
}

if (HOSTS.audio) {
  let context = null;
  let master = null;
  let voices = [];
  const ready = () => {
    if (context === null) {
      const Ctor = globalThis.AudioContext || globalThis.webkitAudioContext;
      if (!Ctor) return null;
      context = new Ctor();
      master = context.createGain();
      master.gain.value = 0.9;
      master.connect(context.destination);
    }
    if (context.state === "suspended") context.resume();
    return context;
  };
  HOSTS.audio = {
    note: (frequency, delay, seconds, gain) => {
      const ctx = ready();
      if (ctx === null) return;
      const at = ctx.currentTime + Math.max(0, delay);
      const osc = ctx.createOscillator();
      const env = ctx.createGain();
      osc.type = "triangle";
      osc.frequency.value = frequency;
      env.gain.setValueAtTime(0.0001, at);
      env.gain.exponentialRampToValueAtTime(Math.max(0.0001, gain), at + 0.02);
      env.gain.exponentialRampToValueAtTime(0.0001, at + Math.max(0.05, seconds));
      osc.connect(env);
      env.connect(master);
      osc.start(at);
      osc.stop(at + Math.max(0.05, seconds) + 0.05);
      voices.push(osc);
      if (voices.length > 256) voices = voices.slice(-128);
    },
    silence: () => {
      for (const osc of voices) {
        try {
          osc.stop();
        } catch (e) {
        }
      }
      voices = [];
    },
    awake: () => {
      const ctx = ready();
      return ctx !== null && ctx.state === "running";
    },
  };
}

function missing(group, name) {
  return () => {
    throw new Error(`no host supplied for @host("${group}") ${name}`);
  };
}

export function provide(group, functions) {
  HOSTS[group] = Object.assign(HOSTS[group] ?? {}, functions);
}

const STRINGS = [
  "Analytical Engine",
  "Ada Lovelace",
  "Ambient",
  "مرحبا بالعالم",
  "أحمد",
  "שלום עולם",
  "שרה",
  "Jazz",
  "日本語のテスト",
  "山田",
  "नमस्ते दुनिया",
  "आर्या",
  "Classical",
  "Difference Machine",
  "Charles Babbage",
  "Silent Partition",
  "Grace Hopper",
  "Tail Call",
  "Guy Steele",
  "All",
  "",
  "search",
  "abcdefghijklmnopqrstuvwxyz",
  ":0",
  ":",
  "app",
  "nav-library",
  "♫",
  "Library",
  "nav-queue",
  "≡",
  "Queue",
  "nav-theme",
  "Theme",
  "rail",
  "☀",
  "☾",
  "main",
  "bar",
  "Search tracks",
  "shuffle",
  "⇄",
  "genre-",
  "chips",
  "list",
  "list-holder",
  "empty",
  "Nothing matches",
  "▷",
  "❚❚",
  "track-",
  "-glyph",
  " · ",
  "np-text",
  "np-title",
  "np-artist",
  " of ",
  "np-transport",
  "prev",
  "◁◁",
  "play",
  "next",
  "▷▷",
  "np-row",
  "np",
  "np-progress",
  "np-holder",
  "▶",
  "Tab",
  "ArrowDown",
  "ArrowUp",
  " ",
  "Enter",
  "playing ",
  ", running ",
  "track 0 opens: ",
  "its first pitch: ",
  "paused after ten frames: ",
  "tab moved focus to ",
  "next: ",
  "jazz leaves ",
  "frames ",
  ", controls ",
  "([{⌈⌊〈⟦⟨⟪⟬⟮〈《「『【〔〖〘〚（［｛｟｢",
  ")]}⌉⌋〉⟧⟩⟫⟭⟯〉》」』】〕〗〙〛）］｝｠｣",
  "W",
  "✓ ",
  "|",
  "-headline",
  "-supporting",
  "-text",
  "-title",
  "-pill",
  "-label",
  "-bar",
  "Backspace",
];

function intern(s) {
  const existing = STRINGS.indexOf(s);
  if (existing !== -1) return existing;
  return STRINGS.push(s) - 1;
}

const S = (i) => STRINGS[i];

export function str(s) {
  return intern(String(s));
}

export function text(i) {
  return S(i);
}

const showInt = (v) => String(v);

const showFloat = (v) =>
  Number.isFinite(v) && Number.isInteger(v) ? v.toFixed(1) : String(v);

const showBool = (v) => (v ? "true" : "false");

const hex = (colour) => '#' + (colour >>> 0).toString(16).padStart(6, '0');
export const FAMILY = 'Roboto, "Helvetica Neue", "Segoe UI", system-ui, sans-serif';
export const NOMINAL_SIZE = 16;

export let fontSize = NOMINAL_SIZE;
export let fontWeight = 400;
export const fontCss = () => fontWeight + ' ' + fontSize + 'px ' + FAMILY;
export function setFont(size, weight) {
  fontSize = size;
  fontWeight = weight;
}

export const FONT = fontCss();

export const textRenderer = {
  rect: (x, y, w, h, colour) =>
    write(
      'rect ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + colour,
    ),
  rrect: (x, y, w, h, r, colour) =>
    write(
      'rrect ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h) + ' ' + showFloat(r) + ' ' + colour,
    ),
  text: (x, y, body, colour) =>
    write('text ' + showFloat(x) + ' ' + showFloat(y) + ' ' + body + ' ' + colour),
  font: (size, weight) => {},
  clip: (x, y, w, h) =>
    write(
      'clip ' + showFloat(x) + ' ' + showFloat(y) + ' ' +
      showFloat(w) + ' ' + showFloat(h),
    ),
  unclip: () => write('unclip'),
  rebuild: (calls) => replay(calls, textRenderer),
};

export function domRenderer(container) {
  container.style.position = 'relative';
  container.replaceChildren();

  let host = container;
  let originX = 0;
  let originY = 0;
  let stack = [];
  let nodes = [];
  let index = 0;

  const place = (el, x, y) => {
    el.style.position = 'absolute';
    el.style.left = x - originX + 'px';
    el.style.top = y - originY + 'px';
    return el;
  };
  const take = () => {
    const el = document.createElement('div');
    nodes[index] = el;
    index += 1;
    host.appendChild(el);
    return el;
  };
  const renderer = {
    rect: (x, y, w, h, colour) => {
      const el = place(take(), x, y);
      el.setAttribute('aria-hidden', 'true');
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.background = hex(colour);
    },
    rrect: (x, y, w, h, r, colour) => {
      const el = place(take(), x, y);
      el.setAttribute('aria-hidden', 'true');
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.background = hex(colour);
      el.style.borderRadius = r + 'px';
    },
    text: (x, y, body, colour) => {
      const el = place(take(), x, y);
      el.style.color = hex(colour);
      el.style.font = fontCss();
      el.style.lineHeight = lineHeight() + 'px';
      el.style.whiteSpace = 'pre';
      el.style.direction = firstStrongRtl(body) ? 'rtl' : 'ltr';
      el.textContent = body;
    },
    clip: (x, y, w, h) => {
      const el = place(take(), x, y);
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      el.style.overflow = 'hidden';
      stack.push([host, originX, originY]);
      host = el;
      originX = x;
      originY = y;
    },
    unclip: () => {
      nodes[index] = null;
      index += 1;
      const popped = stack.pop();
      if (popped) {
        host = popped[0];
        originX = popped[1];
        originY = popped[2];
      }
    },

    rebuild: (calls) => {
      container.replaceChildren();
      host = container;
      originX = 0;
      originY = 0;
      stack = [];
      nodes = [];
      index = 0;
      replay(calls, renderer);
    },

    patch: (previous, next, diff) => {
      for (let i = diff.from; i < diff.newEnd; i += 1) {
        const el = nodes[i];
        const call = next[i];
        const was = previous[i];
        if (!el) continue;
        const ox = Number(el.style.left.replace('px', '')) - was[1];
        const oy = Number(el.style.top.replace('px', '')) - was[2];
        el.style.left = call[1] + ox + 'px';
        el.style.top = call[2] + oy + 'px';
        if (call[0] === 'r') {
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          el.style.background = hex(call[5]);
        } else if (call[0] === 'R') {
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
          el.style.borderRadius = call[5] + 'px';
          el.style.background = hex(call[6]);
        } else if (call[0] === 't') {
          el.style.color = hex(call[4]);
          el.style.direction = firstStrongRtl(call[3]) ? 'rtl' : 'ltr';
          if (el.textContent !== call[3]) el.textContent = call[3];
        } else if (call[0] === 'c') {
          el.style.width = call[3] + 'px';
          el.style.height = call[4] + 'px';
        }
      }
    },
  };
  return renderer;
}

let announcer = null;

export function setAnnouncer(element) {
  announcer = element;
  if (element) element.replaceChildren();
}

let announcing = true;

function announce(body) {
  if (!announcer || !announcing || body === '') return;
  const line = document.createElement('div');
  line.textContent = body;
  announcer.appendChild(line);
}

export function canvasRenderer(ctx) {
  let depth = 0;
  const atlas = glyphAtlas(ctx);
  const renderer = {
    rect: (x, y, w, h, colour) => {
      ctx.fillStyle = hex(colour);
      ctx.fillRect(x, y, w, h);
    },
    rrect: (x, y, w, h, r, colour) => {
      ctx.fillStyle = hex(colour);
      const radius = Math.max(0, Math.min(r, Math.min(w, h) / 2));
      ctx.beginPath();
      if (ctx.roundRect) {
        ctx.roundRect(x, y, w, h, radius);
      } else {
        ctx.moveTo(x + radius, y);
        ctx.arcTo(x + w, y, x + w, y + h, radius);
        ctx.arcTo(x + w, y + h, x, y + h, radius);
        ctx.arcTo(x, y + h, x, y, radius);
        ctx.arcTo(x, y, x + w, y, radius);
      }
      ctx.closePath();
      ctx.fill();
    },
    text: (x, y, body, colour) => {
      announce(body);
      const top = y + baselineOffset();
      if (fontCss() === FONT && atlas && atlas.text(x, top, body, colour)) return;
      ctx.fillStyle = hex(colour);
      ctx.font = fontCss();
      ctx.textBaseline = 'top';
      ctx.textAlign = 'left';
      ctx.direction = firstStrongRtl(body) ? 'rtl' : 'ltr';
      ctx.fillText(body, x, top);
    },
    clip: (x, y, w, h) => {
      ctx.save();
      depth += 1;
      ctx.beginPath();
      ctx.rect(x, y, w, h);
      ctx.clip();
    },
    unclip: () => {
      if (depth > 0) {
        ctx.restore();
        depth -= 1;
      }
    },

    rebuild: (calls) => {
      ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
      setAnnouncer(announcer);
      replay(calls, renderer);
    },

    damage: (calls, rects) => {
      setAnnouncer(announcer);
      for (const call of calls) {
        if (call[0] === 't') announce(call[3]);
      }
      announcing = false;
      for (const rect of rects) {
        ctx.save();
        ctx.beginPath();
        ctx.rect(rect[0], rect[1], rect[2], rect[3]);
        ctx.clip();
        ctx.clearRect(rect[0], rect[1], rect[2], rect[3]);
        for (const call of calls) {
          const bounds = callBounds(call);
          if (bounds === null || rectsOverlap(bounds, rect)) {
            replay([call], renderer);
          }
        }
        ctx.restore();
      }
      announcing = true;
    },
  };
  return renderer;
}

export function firstStrongRtl(body) {
  for (const ch of body) {
    const cp = ch.codePointAt(0);
    if (
      (cp >= 0x0590 && cp <= 0x08ff) ||
      (cp >= 0xfb1d && cp <= 0xfdff) ||
      (cp >= 0xfe70 && cp <= 0xfeff)
    ) {
      return true;
    }
    if ((cp >= 0x41 && cp <= 0x5a) || (cp >= 0x61 && cp <= 0x7a) || (cp >= 0xc0 && cp < 0x0590)) {
      return false;
    }
  }
  return false;
}

export function atlasPlan(body, measureOne) {
  const measurer = measureOne ?? measure;
  const clusters = [];
  let sawRtl = false;
  let sawLtr = false;
  for (const ch of body) {
    const cp = ch.codePointAt(0);
    if (cp > 0xffff) return null;
    if (cp === 0x200d || (cp >= 0xfe00 && cp <= 0xfe0f)) return null;
    if (
      (cp >= 0x200b && cp <= 0x200f) ||
      (cp >= 0x202a && cp <= 0x202e) ||
      (cp >= 0x2060 && cp <= 0x206f) ||
      cp === 0x061c ||
      cp === 0xfeff
    ) {
      return null;
    }
    if (
      (cp >= 0x0600 && cp <= 0x06ff) ||
      (cp >= 0x0750 && cp <= 0x077f) ||
      (cp >= 0x0870 && cp <= 0x08ff)
    ) {
      return null;
    }
    const advance = measurer(ch);
    if (!(advance >= 0)) return null;
    if (advance === 0 && clusters.length > 0) {
      clusters[clusters.length - 1].marks.push(ch);
      continue;
    }
    const rtlChar =
      (cp >= 0x0590 && cp <= 0x05ff) || (cp >= 0xfb1d && cp <= 0xfdff) ||
      (cp >= 0xfe70 && cp <= 0xfefe);
    if (rtlChar) sawRtl = true;
    else if (cp !== 0x20) sawLtr = true;
    clusters.push({ ch, advance, marks: [] });
  }
  if (sawRtl && sawLtr) return null;
  const ordered = sawRtl ? [...clusters].reverse() : clusters;
  let pen = 0;
  const entries = [];
  for (const cluster of ordered) {
    entries.push({ ch: cluster.ch, x: pen });
    for (const mark of cluster.marks) {
      entries.push({ ch: mark, x: pen + cluster.advance });
    }
    pen += cluster.advance;
  }
  if (Math.abs(pen - measurer(body)) > 0.5) return null;
  return entries;
}

function defaultTileMaker(font, scale) {
  if (typeof document === 'undefined') return null;
  const measurer = document.createElement('canvas').getContext('2d');
  measurer.font = font;
  measurer.textBaseline = 'top';
  return (ch, colour) => {
    const m = measurer.measureText(ch);
    if (m.actualBoundingBoxLeft === undefined || m.actualBoundingBoxRight === undefined) {
      return null;
    }
    const left = Math.ceil(Math.max(0, m.actualBoundingBoxLeft)) + 1;
    const top = Math.ceil(Math.max(0, m.actualBoundingBoxAscent ?? 0)) + 1;
    const w = left + Math.ceil(Math.max(m.actualBoundingBoxRight, m.width)) + 2;
    const h = top + Math.ceil(Math.max(0, m.actualBoundingBoxDescent ?? lineHeight())) + 2;
    const tile = document.createElement('canvas');
    tile.width = Math.max(1, Math.ceil(w * scale));
    tile.height = Math.max(1, Math.ceil(h * scale));
    const tctx = tile.getContext('2d');
    tctx.scale(scale, scale);
    tctx.font = font;
    tctx.textBaseline = 'top';
    tctx.fillStyle = colour;
    tctx.fillText(ch, left, top);
    return { canvas: tile, left, top, w, h };
  };
}

export function glyphAtlas(ctx, font = FONT, makeTile = null) {
  const scale = (ctx.getTransform ? ctx.getTransform().a : 1) || 1;
  const rasterise = makeTile ?? defaultTileMaker(font, scale);
  if (!rasterise) return null;
  const tiles = new Map();
  let rasterised = 0;
  let reused = 0;
  let fallbacks = 0;
  const tileFor = (ch, colour) => {
    const key = ch + '\0' + font + '\0' + colour;
    let tile = tiles.get(key);
    if (tile === undefined) {
      if (tiles.size >= 4096) tiles.clear();
      tile = rasterise(ch, hex(colour));
      tiles.set(key, tile);
      if (tile) rasterised += 1;
    } else if (tile) {
      reused += 1;
    }
    return tile;
  };
  return {
    stats: () => ({ tiles: tiles.size, rasterised, reused, fallbacks }),
    text: (x, y, body, colour) => {
      const plan = atlasPlan(body, measure);
      if (plan === null) {
        fallbacks += 1;
        return false;
      }
      const placed = [];
      for (const glyph of plan) {
        const tile = tileFor(glyph.ch, colour);
        if (!tile) {
          fallbacks += 1;
          return false;
        }
        placed.push([tile, glyph.x]);
      }
      for (const [tile, gx] of placed) {
        const px = Math.round((x + gx - tile.left) * scale) / scale;
        const py = Math.round((y - tile.top) * scale) / scale;
        ctx.drawImage(tile.canvas, px, py, tile.w, tile.h);
      }
      return true;
    },
  };
}

const NOMINAL_ADVANCE = 8;
export let measure = (body) =>
  [...body].length * NOMINAL_ADVANCE * (fontSize / NOMINAL_SIZE);

export function setMeasure(fn) {
  measure = fn;
}

export let lineHeight = () => NOMINAL_SIZE * (fontSize / NOMINAL_SIZE);

export function setLineHeight(fn) {
  lineHeight = fn;
}

const BASELINES = new Map();

export function baselineOffset() {
  const key = fontCss();
  const cached = BASELINES.get(key);
  if (cached !== undefined) return cached;
  if (typeof document === 'undefined') return 0;
  const ctx = document.createElement('canvas').getContext('2d');
  ctx.font = key;
  ctx.textBaseline = 'top';
  const fromTop = -(ctx.measureText('Mg').alphabeticBaseline ?? 0);
  const probe = document.createElement('div');
  probe.style.cssText =
    'position:absolute;visibility:hidden;white-space:pre;font:' + key +
    ';line-height:' + lineHeight() + 'px';
  probe.textContent = 'Mg';
  const marker = document.createElement('span');
  marker.style.cssText = 'display:inline-block;width:0;height:0;vertical-align:baseline';
  probe.appendChild(marker);
  document.body.appendChild(probe);
  const domFromTop = marker.getBoundingClientRect().top - probe.getBoundingClientRect().top;
  probe.remove();
  const offset = Number.isFinite(domFromTop - fromTop) ? domFromTop - fromTop : 0;
  BASELINES.set(key, offset);
  return offset;
}

export function fontMeasure() {
  const ctx = document.createElement('canvas').getContext('2d');
  return (body) => {
    ctx.font = fontCss();
    return ctx.measureText(body).width;
  };
}

export function fontLineHeight() {
  const ctx = document.createElement('canvas').getContext('2d');
  return () => {
    ctx.font = fontCss();
    const m = ctx.measureText('Mg');
    const ascent = m.fontBoundingBoxAscent ?? m.actualBoundingBoxAscent;
    const descent = m.fontBoundingBoxDescent ?? m.actualBoundingBoxDescent;
    const height = (ascent ?? 0) + (descent ?? 0);
    return Number.isFinite(height) && height > 0 ? height : fontSize;
  };
}

export let renderer = textRenderer;

export function setRenderer(r) {
  renderer = r;
}

export let write = (line) => console.log(line);

export function setWriter(fn) {
  write = fn;
}

function imports() {
  return {
    ...HOSTS,
    kite: {
      print_int: (v) => write(showInt(v)),
      print_float: (v) => write(showFloat(v)),
      print_bool: (v) => write(showBool(v)),
      print_str: (i) => write(S(i)),
      str_concat: (a, b) => intern(S(a) + S(b)),
      str_eq: (a, b) => (S(a) === S(b) ? 1 : 0),
      str_compare: (a, b) =>
        S(a) < S(b) ? -1n : S(a) > S(b) ? 1n : 0n,
      draw_rect: (x, y, w, h, colour) => renderer.rect(x, y, w, h, Number(colour)),
      draw_rrect: (x, y, w, h, r, colour) => renderer.rrect(x, y, w, h, r, Number(colour)),
      draw_text: (x, y, i, colour) => renderer.text(x, y, S(i), Number(colour)),
      draw_clip: (x, y, w, h) => renderer.clip(x, y, w, h),
      draw_unclip: () => renderer.unclip(),
      measure_text: (i) => measure(S(i)),
      line_height: () => lineHeight(),
      draw_font: (size, weight) => {
        setFont(size, Number(weight));
        if (renderer.font) renderer.font(size, Number(weight));
      },
      str_slice: (i, from, to) => {
        const cs = [...S(i)];
        const a = Math.min(Math.max(Number(from), 0), cs.length);
        const b = Math.min(Math.max(Number(to), a), cs.length);
        return intern(cs.slice(a, b).join(''));
      },
      str_index_of: (i, n) => {
        const at = S(i).indexOf(S(n));
        return at < 0 ? -1n : BigInt([...S(i).slice(0, at)].length);
      },
      str_trim: (i) => intern(S(i).trim()),
      str_code_at: (i, at) => {
        const c = [...S(i)][Number(at)];
        return c === undefined ? -1n : BigInt(c.codePointAt(0));
      },
      str_of_int: (v) => intern(showInt(v)),
      str_of_float: (v) => intern(showFloat(v)),
      str_of_bool: (v) => intern(showBool(v)),
      str_len: (i) => BigInt([...S(i)].length),
      task_spawn: (poll) => {
        TASKS.push({ poll, wakeAt: null, parked: false, waitingOnHost: false });
      },
      task_wake_at: (ms) => {
        wakeRequest = Number(ms);
      },
      task_park: () => {
        parkRequest = true;
      },
      task_wait_host: () => {
        hostWaitRequest = true;
      },
      time_now: () => BigInt(clock),
    },
  };
}

export function recordingRenderer() {
  const calls = [];
  let size = NOMINAL_SIZE;
  let weight = 400;
  return {
    calls,
    rect: (x, y, w, h, colour) => calls.push(['r', x, y, w, h, colour]),
    rrect: (x, y, w, h, r, colour) => calls.push(['R', x, y, w, h, r, colour]),
    font: (s, w) => {
      size = s;
      weight = w;
    },
    text: (x, y, body, colour) => calls.push(['t', x, y, body, colour, size, weight]),
    clip: (x, y, w, h) => calls.push(['c', x, y, w, h]),
    unclip: () => calls.push(['u']),
  };
}

export function replay(calls, renderer) {
  let size = NOMINAL_SIZE;
  let weight = 400;
  for (const call of calls) {
    if (call[0] === 'r') renderer.rect(call[1], call[2], call[3], call[4], call[5]);
    else if (call[0] === 'R')
      renderer.rrect(call[1], call[2], call[3], call[4], call[5], call[6]);
    else if (call[0] === 't') {
      const want = call[5] ?? NOMINAL_SIZE;
      const wantWeight = call[6] ?? 400;
      if (want !== size || wantWeight !== weight) {
        size = want;
        weight = wantWeight;
        setFont(size, weight);
        if (renderer.font) renderer.font(size, weight);
      }
      renderer.text(call[1], call[2], call[3], call[4]);
    } else if (call[0] === 'c') renderer.clip(call[1], call[2], call[3], call[4]);
    else renderer.unclip();
  }
}

export function sameCall(a, b) {
  if (a === undefined || b === undefined || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export function sameFrame(a, b) {
  if (a === null || b === null || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (!sameCall(a[i], b[i])) return false;
  }
  return true;
}

export function callBounds(call) {
  if (call[0] === 'r' || call[0] === 'R') return [call[1], call[2], call[3], call[4]];
  if (call[0] === 't') {
    const size = fontSize;
    const weight = fontWeight;
    setFont(call[5] ?? NOMINAL_SIZE, call[6] ?? 400);
    const box = [call[1], call[2], measure(call[3]), lineHeight()];
    setFont(size, weight);
    return box;
  }
  return null;
}

export function diffFrames(previous, next) {
  const old = previous ?? [];
  let from = 0;
  while (from < old.length && from < next.length && sameCall(old[from], next[from])) {
    from += 1;
  }
  let oldEnd = old.length;
  let newEnd = next.length;
  while (oldEnd > from && newEnd > from && sameCall(old[oldEnd - 1], next[newEnd - 1])) {
    oldEnd -= 1;
    newEnd -= 1;
  }
  return {
    same: previous !== null && from === oldEnd && from === newEnd,
    from,
    oldEnd,
    newEnd,
    patchable:
      previous !== null &&
      old.length === next.length &&
      old.every((call, i) => call[0] === next[i][0]),
  };
}

export function damageOf(previous, next, diff, limit) {
  const cap = limit ?? 16;
  const old = previous ?? [];
  let rects = [];
  for (let i = diff.from; i < diff.oldEnd; i += 1) {
    const r = callBounds(old[i]);
    if (r) rects.push(r);
  }
  for (let i = diff.from; i < diff.newEnd; i += 1) {
    const r = callBounds(next[i]);
    if (r) rects.push(r);
  }
  const structural = (call) => call[0] === 'c' || call[0] === 'u';
  for (let i = diff.from; i < diff.oldEnd; i += 1) {
    if (structural(old[i])) return null;
  }
  for (let i = diff.from; i < diff.newEnd; i += 1) {
    if (structural(next[i])) return null;
  }
  if (rects.length === 0) return [];
  rects = mergeRects(rects);
  if (rects.length > cap) return [boundingBox(rects)];
  return rects;
}

export function rectsOverlap(a, b) {
  return (
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
  );
}

function boundingBox(rects) {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const r of rects) {
    x0 = Math.min(x0, r[0]);
    y0 = Math.min(y0, r[1]);
    x1 = Math.max(x1, r[0] + r[2]);
    y1 = Math.max(y1, r[1] + r[3]);
  }
  return [x0, y0, x1 - x0, y1 - y0];
}

function mergeRects(rects) {
  const out = [];
  for (const rect of rects) {
    let merged = rect;
    let again = true;
    while (again) {
      again = false;
      for (let i = out.length - 1; i >= 0; i -= 1) {
        if (rectsOverlap(out[i], merged)) {
          merged = boundingBox([out[i], merged]);
          out.splice(i, 1);
          again = true;
        }
      }
    }
    out.push(merged);
  }
  return out;
}

const TASKS = [];
let clock = 0;
let wakeRequest = null;
let parkRequest = false;
let hostWaitRequest = false;

export async function drive(exports) {
  while (TASKS.length > 0) {
    let polled = false;
    let completed = false;
    for (let i = 0; i < TASKS.length; ) {
      const task = TASKS[i];
      if (task.parked || task.waitingOnHost || (task.wakeAt !== null && task.wakeAt > clock)) {
        i += 1;
        continue;
      }
      polled = true;
      task.wakeAt = null;
      wakeRequest = null;
      parkRequest = false;
      hostWaitRequest = false;
      const done = exports.kite_poll(task.poll) !== 0;
      if (TASKS[i] === task) {
        task.wakeAt = wakeRequest;
        task.parked = parkRequest;
        task.waitingOnHost = hostWaitRequest;
      }
      if (done) {
        TASKS.splice(i, 1);
        completed = true;
      } else {
        i += 1;
      }
    }
    if (completed) {
      for (const t of TASKS) {
        t.parked = false;
        t.wakeAt = null;
      }
    }
    if (!polled) {
      if (TASKS.some((t) => t.waitingOnHost)) {
        await new Promise((resolve) => setTimeout(resolve, 0));
        for (const t of TASKS) t.waitingOnHost = false;
        continue;
      }
      const next = TASKS.reduce(
        (best, t) => (t.wakeAt !== null && (best === null || t.wakeAt < best) ? t.wakeAt : best),
        null,
      );
      if (next === null || next <= clock) {
        throw new Error(TASKS.length + ' task(s) can never make progress');
      }
      clock = next;
    }
  }
}

export async function instantiate(source = "app.wasm") {
  const bytes =
    source instanceof Uint8Array
      ? source
      : new Uint8Array(await (await fetch(source)).arrayBuffer());
  const { instance } = await WebAssembly.instantiate(bytes, imports());
  return instance.exports;
}

export async function run(source) {
  const exports = await instantiate(source);
  if (typeof exports.main !== "function") {
    throw new Error("this module has no `main`");
  }
  const result = exports.main();
  if (typeof exports.kite_poll === "function") {
    await drive(exports);
  }
  return result;
}

export const EVENT_CLICK = 0n;
export const EVENT_KEY = 1n;
export const EVENT_WHEEL = 2n;
export const EVENT_MOVE = 3n;
export const EVENT_DOWN = 4n;
export const EVENT_UP = 5n;
export const EVENT_RESIZE = 6n;
export const EVENT_FRAME = 7n;

export function isApplication(exports) {
  return ["init", "view", "update"].every((n) => typeof exports[n] === "function");
}
