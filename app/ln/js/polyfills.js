// The browser pieces LNReader plugins use that QuickJS doesn't ship.

globalThis.global = globalThis;
globalThis.window = globalThis;
globalThis.self = globalThis;

const logAt = (level) => (...args) => __log(level, args.map((a) => (typeof a === 'string' ? a : safeJson(a))).join(' '));
function safeJson(v) {
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}
globalThis.console = { log: logAt('log'), info: logAt('info'), warn: logAt('warn'), error: logAt('error'), debug: logAt('debug') };

// Timers. Rust sleeps until the next one is due, then runs it.
const timers = [];
let timerId = 1;
globalThis.setTimeout = (fn, ms = 0, ...args) => {
  const id = timerId++;
  timers.push({ id, at: Date.now() + Math.max(0, +ms || 0), fn, args });
  return id;
};
globalThis.clearTimeout = (id) => {
  const i = timers.findIndex((t) => t.id === id);
  if (i >= 0) timers.splice(i, 1);
};
globalThis.setInterval = (fn, ms = 0, ...args) => {
  const id = timerId++;
  const tick = () => {
    timers.push({ id, at: Date.now() + Math.max(1, +ms || 0), fn: tick, args: [] });
    fn(...args);
  };
  timers.push({ id, at: Date.now() + Math.max(1, +ms || 0), fn: tick, args: [] });
  return id;
};
globalThis.clearInterval = globalThis.clearTimeout;
globalThis.queueMicrotask = (fn) => Promise.resolve().then(fn);
// Milliseconds until the next timer is due, or -1 when there is none.
globalThis.__nextTimer = () => (timers.length ? Math.max(0, Math.min(...timers.map((t) => t.at)) - Date.now()) : -1);
globalThis.__runTimer = () => {
  timers.sort((a, b) => a.at - b.at);
  const t = timers.shift();
  if (t) t.fn(...t.args);
};

// Text encoding, UTF-8 only.
globalThis.TextEncoder = class TextEncoder {
  get encoding() {
    return 'utf-8';
  }
  encode(s = '') {
    const out = [];
    for (const ch of String(s)) {
      const c = ch.codePointAt(0);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    }
    return new Uint8Array(out);
  }
};
globalThis.TextDecoder = class TextDecoder {
  constructor(label = 'utf-8') {
    this.encoding = String(label).toLowerCase();
  }
  decode(buf) {
    if (!buf) return '';
    const b = buf instanceof ArrayBuffer ? new Uint8Array(buf) : new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
    let s = '';
    for (let i = 0; i < b.length; ) {
      const c = b[i++];
      let cp = c;
      if (c >= 0xf0) cp = ((c & 7) << 18) | ((b[i++] & 63) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
      else if (c >= 0xe0) cp = ((c & 15) << 12) | ((b[i++] & 63) << 6) | (b[i++] & 63);
      else if (c >= 0xc0) cp = ((c & 31) << 6) | (b[i++] & 63);
      s += String.fromCodePoint(cp);
    }
    return s;
  }
};

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
globalThis.btoa = (s) => {
  s = String(s);
  let out = '';
  for (let i = 0; i < s.length; i += 3) {
    const [a, b, c] = [s.charCodeAt(i), s.charCodeAt(i + 1), s.charCodeAt(i + 2)];
    out += B64[a >> 2] + B64[((a & 3) << 4) | ((b || 0) >> 4)];
    out += i + 1 < s.length ? B64[((b & 15) << 2) | ((c || 0) >> 6)] : '=';
    out += i + 2 < s.length ? B64[c & 63] : '=';
  }
  return out;
};
globalThis.atob = (s) => {
  s = String(s).replace(/[\s=]+/g, '');
  let out = '';
  let bits = 0;
  let acc = 0;
  for (const ch of s) {
    acc = (acc << 6) | B64.indexOf(ch);
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out += String.fromCharCode((acc >> bits) & 255);
    }
  }
  return out;
};

const enc = (s) => encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase());
const dec = (s) => {
  try {
    return decodeURIComponent(s.replace(/\+/g, ' '));
  } catch {
    return s;
  }
};

class URLSearchParams {
  constructor(init = '') {
    this._l = [];
    if (init instanceof URLSearchParams) this._l = init._l.map((p) => [...p]);
    else if (Array.isArray(init)) this._l = init.map(([k, v]) => [String(k), String(v)]);
    else if (init && typeof init === 'object') this._l = Object.entries(init).map(([k, v]) => [k, String(v)]);
    else {
      const s = String(init).replace(/^\?/, '');
      if (s) this._l = s.split('&').filter(Boolean).map((p) => {
        const i = p.indexOf('=');
        return i < 0 ? [dec(p), ''] : [dec(p.slice(0, i)), dec(p.slice(i + 1))];
      });
    }
  }
  append(k, v) {
    this._l.push([String(k), String(v)]);
    this._u?.();
  }
  delete(k) {
    this._l = this._l.filter((p) => p[0] !== k);
    this._u?.();
  }
  get(k) {
    return this._l.find((p) => p[0] === k)?.[1] ?? null;
  }
  getAll(k) {
    return this._l.filter((p) => p[0] === k).map((p) => p[1]);
  }
  has(k) {
    return this._l.some((p) => p[0] === k);
  }
  set(k, v) {
    const i = this._l.findIndex((p) => p[0] === k);
    if (i < 0) this._l.push([String(k), String(v)]);
    else {
      this._l[i][1] = String(v);
      this._l = this._l.filter((p, j) => p[0] !== k || j === i);
    }
    this._u?.();
  }
  sort() {
    this._l.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    this._u?.();
  }
  forEach(fn, self) {
    this._l.forEach(([k, v]) => fn.call(self, v, k, this));
  }
  *entries() {
    yield* this._l.map((p) => [...p]);
  }
  *keys() {
    yield* this._l.map((p) => p[0]);
  }
  *values() {
    yield* this._l.map((p) => p[1]);
  }
  [Symbol.iterator]() {
    return this.entries();
  }
  get size() {
    return this._l.length;
  }
  toString() {
    return this._l.map(([k, v]) => enc(k) + '=' + enc(v)).join('&');
  }
}
globalThis.URLSearchParams = URLSearchParams;

const DEFAULT_PORT = { 'http:': '80', 'https:': '443', 'ftp:': '21', 'ws:': '80', 'wss:': '443' };
function dotted(path) {
  const out = [];
  for (const seg of path.split('/')) {
    if (seg === '..') out.length > 1 && out.pop();
    else if (seg !== '.') out.push(seg);
  }
  const last = path.split('/').pop();
  if (last === '.' || last === '..') out.push('');
  return out.join('/');
}
class URL {
  constructor(input, base) {
    input = String(input).trim();
    let m = /^([a-zA-Z][a-zA-Z0-9+.-]*):(\/\/)?([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/.exec(input);
    if (m && !m[2] && !/^(https?|ftp|wss?|file)$/i.test(m[1])) m = [input, m[1], '', '', m[3] + m[4], m[5], m[6]];
    if (!m || (m[2] === undefined && /^(https?|ftp|wss?)$/i.test(m[1]) && base)) {
      if (base === undefined) throw new TypeError('Invalid URL: ' + input);
      const b = base instanceof URL ? base : new URL(base);
      let path;
      let search = '';
      let hash = '';
      let rest = input;
      const h = rest.indexOf('#');
      if (h >= 0) {
        hash = rest.slice(h);
        rest = rest.slice(0, h);
      }
      const q = rest.indexOf('?');
      if (q >= 0) {
        search = rest.slice(q);
        rest = rest.slice(0, q);
      }
      if (rest.startsWith('//')) return new URL(b.protocol + input);
      if (rest === '') {
        path = b.pathname;
        if (q < 0) search = b.search;
      } else if (rest.startsWith('/')) path = rest;
      else path = b.pathname.replace(/[^/]*$/, '') + rest;
      return new URL(b.protocol + '//' + b.host + dotted(path) + search + hash);
    }
    this.protocol = m[1].toLowerCase() + ':';
    const auth = m[3];
    const at = auth.lastIndexOf('@');
    const cred = at >= 0 ? auth.slice(0, at) : '';
    const hostport = at >= 0 ? auth.slice(at + 1) : auth;
    [this.username, this.password = ''] = cred.split(':');
    this.username = this.username || '';
    const pm = /^(.*?)(?::(\d*))?$/.exec(hostport);
    this.hostname = (pm[1] || '').toLowerCase();
    this.port = pm[2] && pm[2] !== DEFAULT_PORT[this.protocol] ? pm[2] : '';
    let path = m[4] || '';
    if (m[2] !== undefined && /^(https?|ftp|wss?|file)/.test(this.protocol)) path = dotted(path || '/') || '/';
    this.pathname = path.replace(/ /g, '%20');
    this.search = m[5] && m[5] !== '?' ? m[5].replace(/ /g, '%20') : '';
    this.hash = m[6] && m[6] !== '#' ? m[6] : '';
    this.searchParams = new URLSearchParams(this.search);
    this.searchParams._u = () => {
      const s = this.searchParams.toString();
      this.search = s ? '?' + s : '';
    };
  }
  get host() {
    return this.hostname + (this.port ? ':' + this.port : '');
  }
  get origin() {
    return /^(https?|ftp|wss?)/.test(this.protocol) ? this.protocol + '//' + this.host : 'null';
  }
  get href() {
    const cred = this.username ? this.username + (this.password ? ':' + this.password : '') + '@' : '';
    return this.protocol + (this.hostname || /^(https?|ftp|wss?|file)/.test(this.protocol) ? '//' : '') + cred + this.host + this.pathname + this.search + this.hash;
  }
  toString() {
    return this.href;
  }
  toJSON() {
    return this.href;
  }
  static canParse(u, b) {
    try {
      new URL(u, b);
      return true;
    } catch {
      return false;
    }
  }
}
globalThis.URL = URL;

class Headers {
  constructor(init) {
    this._m = new Map();
    if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
    else if (Array.isArray(init)) init.forEach(([k, v]) => this.append(k, v));
    else if (init) Object.entries(init).forEach(([k, v]) => this.set(k, v));
  }
  append(k, v) {
    k = String(k).toLowerCase();
    this._m.set(k, this._m.has(k) ? this._m.get(k) + ', ' + v : String(v));
  }
  set(k, v) {
    this._m.set(String(k).toLowerCase(), String(v));
  }
  get(k) {
    return this._m.get(String(k).toLowerCase()) ?? null;
  }
  has(k) {
    return this._m.has(String(k).toLowerCase());
  }
  delete(k) {
    this._m.delete(String(k).toLowerCase());
  }
  forEach(fn, self) {
    this._m.forEach((v, k) => fn.call(self, v, k, this));
  }
  *entries() {
    yield* this._m.entries();
  }
  *keys() {
    yield* this._m.keys();
  }
  *values() {
    yield* this._m.values();
  }
  [Symbol.iterator]() {
    return this.entries();
  }
}
globalThis.Headers = Headers;

class FormData {
  constructor() {
    this._l = [];
  }
  append(k, v) {
    this._l.push([String(k), v]);
  }
  set(k, v) {
    this.delete(k);
    this.append(k, v);
  }
  delete(k) {
    this._l = this._l.filter((p) => p[0] !== k);
  }
  get(k) {
    return this._l.find((p) => p[0] === k)?.[1] ?? null;
  }
  getAll(k) {
    return this._l.filter((p) => p[0] === k).map((p) => p[1]);
  }
  has(k) {
    return this._l.some((p) => p[0] === k);
  }
  forEach(fn) {
    this._l.forEach(([k, v]) => fn(v, k, this));
  }
  *entries() {
    yield* this._l;
  }
  [Symbol.iterator]() {
    return this.entries();
  }
}
globalThis.FormData = FormData;

class AbortSignal {
  constructor() {
    this.aborted = false;
    this.reason = undefined;
    this._f = [];
  }
  addEventListener(t, fn) {
    if (t === 'abort') this._f.push(fn);
  }
  removeEventListener(t, fn) {
    this._f = this._f.filter((f) => f !== fn);
  }
  throwIfAborted() {
    if (this.aborted) throw this.reason;
  }
  static timeout(ms) {
    const c = new AbortController();
    setTimeout(() => c.abort(new Error('The operation timed out')), ms);
    return c.signal;
  }
}
globalThis.AbortSignal = AbortSignal;
globalThis.AbortController = class AbortController {
  constructor() {
    this.signal = new AbortSignal();
  }
  abort(reason = new Error('This operation was aborted')) {
    if (this.signal.aborted) return;
    this.signal.aborted = true;
    this.signal.reason = reason;
    this.signal._f.forEach((f) => f({ type: 'abort' }));
    this.signal.onabort?.({ type: 'abort' });
  }
};

class Response {
  constructor(body, init = {}) {
    this._b = body ?? '';
    this.status = init.status ?? 200;
    this.statusText = init.statusText ?? '';
    this.url = init.url ?? '';
    this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
    this.redirected = false;
    this.type = 'basic';
  }
  get ok() {
    return this.status >= 200 && this.status < 300;
  }
  async text() {
    return this._b;
  }
  async json() {
    return JSON.parse(this._b);
  }
  async arrayBuffer() {
    return new TextEncoder().encode(this._b).buffer;
  }
  clone() {
    return new Response(this._b, this);
  }
}
globalThis.Response = Response;

// multipart/form-data with text fields only.
function multipart(fd) {
  const boundary = '----nano' + Math.random().toString(16).slice(2);
  let body = '';
  for (const [k, v] of fd._l) body += `--${boundary}\r\nContent-Disposition: form-data; name="${k}"\r\n\r\n${v}\r\n`;
  return { body: body + `--${boundary}--\r\n`, type: `multipart/form-data; boundary=${boundary}` };
}

globalThis.fetch = async (input, init = {}) => {
  const url = typeof input === 'string' || input instanceof URL ? String(input) : input.url;
  const headers = new Headers(typeof input === 'object' && input.headers ? input.headers : undefined);
  new Headers(init.headers).forEach((v, k) => headers.set(k, v));
  let body = init.body;
  if (body instanceof URLSearchParams) {
    body = body.toString();
    if (!headers.has('content-type')) headers.set('content-type', 'application/x-www-form-urlencoded;charset=UTF-8');
  } else if (body instanceof FormData) {
    const m = multipart(body);
    body = m.body;
    headers.set('content-type', m.type);
  } else if (body !== undefined && body !== null && typeof body !== 'string') {
    body = String(body);
  }
  if (init.signal?.aborted) throw init.signal.reason;
  const req = { url, method: (init.method || 'GET').toUpperCase(), headers: Object.fromEntries(headers), body: body ?? null };
  const res = JSON.parse(__fetch(JSON.stringify(req)));
  if (res.error) throw new TypeError(res.error);
  return new Response(res.body, { status: res.status, statusText: res.statusText, url: res.url, headers: res.headers });
};

// Just enough Intl for plugins that ask for the time zone.
globalThis.Intl = globalThis.Intl || {
  DateTimeFormat: () => ({ resolvedOptions: () => ({ timeZone: 'UTC', locale: 'en-US' }), format: (d) => new Date(d).toISOString() }),
  NumberFormat: () => ({ format: (n) => String(n) }),
};
