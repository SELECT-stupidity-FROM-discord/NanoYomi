// What LNReader hands a plugin through `require`, and the entry points Rust calls.

const novelStatus = { Unknown: 'Unknown', Ongoing: 'Ongoing', Completed: 'Completed', Licensed: 'Licensed', PublishingFinished: 'Publishing Finished', Cancelled: 'Cancelled', OnHiatus: 'On Hiatus' };
const FilterTypes = { TextInput: 'Text', Picker: 'Picker', CheckboxGroup: 'Checkbox', Switch: 'Switch', ExcludableCheckboxGroup: 'XCheckbox' };

const defaultHeaders = { 'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36' };
const fetchApi = async (url, init = {}) => fetch(url, { ...init, headers: { ...defaultHeaders, ...Object.fromEntries(new Headers(init.headers)) } });
const fetchText = async (url, init) => (await fetchApi(url, init)).text();
const fetchFile = async () => {
  throw new Error('fetchFile is not supported');
};
const fetchProto = async () => {
  throw new Error('fetchProto is not supported');
};

const store = {
  get: (k) => {
    const v = __storage('get', k);
    return v === undefined || v === null ? undefined : JSON.parse(v);
  },
  set: (k, v) => __storage('set', k, JSON.stringify(v)),
  delete: (k) => __storage('delete', k),
  clearAll: () => __storage('clear'),
  getAllKeys: () => JSON.parse(__storage('keys')),
};
const mem = () => {
  const m = new Map();
  return { get: (k) => m.get(k), set: (k, v) => m.set(k, v), delete: (k) => m.delete(k), clearAll: () => m.clear(), getAllKeys: () => [...m.keys()] };
};

const libs = {
  '@libs/fetch': { fetchApi, fetchText, fetchFile, fetchProto },
  '@libs/novelStatus': { NovelStatus: novelStatus },
  '@libs/filterInputs': { FilterTypes },
  '@libs/isAbsoluteUrl': { isUrlAbsolute: (u) => /^([a-z][a-z0-9+.-]*:)?\/\//i.test(u) },
  '@libs/defaultCover': { defaultCover: '' },
  '@libs/aes': {
    gcm: {
      decrypt: () => {
        throw new Error('This source needs AES decryption, which Nano does not support yet');
      },
    },
  },
  '@libs/storage': { storage: store, localStorage: store, sessionStorage: mem() },
};
export const install = ({ cheerio, Parser, dayjs }) => Object.assign(libs, { cheerio, htmlparser2: { Parser }, dayjs });

const require = (name) => {
  if (!(name in libs)) throw new Error(`Plugin needs "${name}", which Nano doesn't provide`);
  return libs[name];
};

let plugin;

// Loads the plugin's source and says who it is.
globalThis.__load = (code) => {
  const module = { exports: {} };
  new Function('require', 'module', 'exports', code)(require, module, module.exports);
  plugin = module.exports.default ?? module.exports;
  if (typeof plugin === 'function') plugin = new plugin();
  const { id, name, version, site, icon, lang, filters, pluginSettings, customJS, customCSS, imageRequestInit, webStorageUtilized } = plugin;
  return JSON.stringify({ id, name, version, site, icon, lang, filters, pluginSettings, customJS, customCSS, imageRequestInit, webStorageUtilized });
};

// Calls a plugin method with JSON arguments and answers with JSON.
globalThis.__call = async (method, args) => {
  if (typeof plugin[method] !== 'function') throw new Error(`This plugin has no ${method}`);
  const result = await plugin[method](...JSON.parse(args));
  return JSON.stringify(result === undefined ? null : result);
};
