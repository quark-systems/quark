#!/usr/bin/env node
// Serves the built desktop app (app/dist) and proxies /v1 (HTTP and the
// /v1/events WebSocket) to a daemon, all on one origin, so the app can talk
// to a quarkd on any port without quarkd's CORS list naming it.
//
//   node serve.mjs --port 24201 --daemon http://127.0.0.1:24200 --dist app/dist
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import path from "node:path";

const args = process.argv.slice(2);
const arg = (name) => {
  const i = args.indexOf(name);
  if (i < 0 || !args[i + 1]) throw new Error(`serve.mjs: ${name} is required`);
  return args[i + 1];
};
const port = Number(arg("--port"));
const daemon = new URL(arg("--daemon"));
const dist = path.resolve(arg("--dist"));
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml",
  ".png": "image/png", ".json": "application/json", ".woff2": "font/woff2", ".wasm": "application/wasm" };

const server = http.createServer((req, res) => {
  if (req.url.startsWith("/v1/") || req.url === "/v1") {
    const up = http.request({ host: daemon.hostname, port: daemon.port, method: req.method, path: req.url,
      headers: { ...req.headers, host: daemon.host } }, (r) => {
      res.writeHead(r.statusCode ?? 502, r.headers);
      r.pipe(res);
    });
    up.on("error", (e) => { res.writeHead(502); res.end(`daemon unreachable: ${e.message}`); });
    req.pipe(up);
    return;
  }
  const rel = decodeURIComponent(new URL(req.url, "http://x").pathname);
  let file = path.join(dist, rel);
  if (!file.startsWith(dist) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(dist, "index.html");
  res.writeHead(200, { "content-type": types[path.extname(file)] ?? "application/octet-stream" });
  fs.createReadStream(file).pipe(res);
});

// WebSocket upgrades are piped byte for byte to the daemon.
server.on("upgrade", (req, sock, head) => {
  const up = net.connect(Number(daemon.port), daemon.hostname, () => {
    const lines = [`${req.method} ${req.url} HTTP/1.1`];
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      const [k, v] = [req.rawHeaders[i], req.rawHeaders[i + 1]];
      lines.push(`${k}: ${k.toLowerCase() === "host" ? daemon.host : v}`);
    }
    up.write(lines.join("\r\n") + "\r\n\r\n");
    if (head.length) up.write(head);
    sock.pipe(up).pipe(sock);
  });
  const close = () => { sock.destroy(); up.destroy(); };
  up.on("error", close);
  sock.on("error", close);
});

server.listen(port, "127.0.0.1", () => console.log(`serve.mjs: app on http://127.0.0.1:${port}/ -> daemon ${daemon.origin}`));
