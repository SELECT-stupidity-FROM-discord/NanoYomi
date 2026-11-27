# Light novel runtime

`runtime.js` is what Nano loads into QuickJS before each LNReader plugin. It is built from `js/` and committed, so building Nano needs no Node.

To rebuild it after changing `js/`:

```
cd app/ln/js
npm install
npm run build
```

- `polyfills.js` is the browser pieces QuickJS lacks: `URL`, `FormData`, timers, `fetch` and so on.
- `glue.js` is what plugins get from `require`, such as `@libs/fetch` and `@libs/novelStatus`.
- `entry.js` bundles cheerio, htmlparser2 and dayjs with them.

Plugins that need `@libs/aes` load but fail when they try to decrypt. Protobuf (`fetchProto`) isn't supported either.
