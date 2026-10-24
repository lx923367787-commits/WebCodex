import { createServer } from "vite";
import { spawn } from "node:child_process";
import { mkdir, access } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("../", import.meta.url));
const executable = path.join(root, "src-tauri", "target", "debug", process.platform === "win32" ? "ai-office-preview.exe" : "ai-office-preview");
const dataDir = path.join(root, ".ai-office-preview");
const port = Number(process.env.AI_OFFICE_PREVIEW_PORT ?? 4179);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error("Invalid AI_OFFICE_PREVIEW_PORT");
const origin = `http://127.0.0.1:${port}`;
await access(executable);
await mkdir(dataDir, { recursive: true });
let queue = Promise.resolve();
let pending = 0;
let gatewayBlocked = false;

function rpc(body) {
  return new Promise((resolve, reject) => {
    if (gatewayBlocked) return reject(new Error("上一个工作进程尚未确认退出，请重启预览服务。"));
    const child = spawn(executable, ["--data-dir", dataDir], {
      cwd: dataDir, windowsHide: true, detached: process.platform !== "win32", stdio: ["pipe", "pipe", "pipe"],
    });
    let output = Buffer.alloc(0);
    let finished = false;
    let terminalError = null;
    let childClosed = false;
    let cleanupConfirmed = false;
    let cleanupDeadline;
    const stop = () => {
      if (!child.pid) return Promise.resolve(true);
      if (process.platform === "win32") {
        return new Promise((done) => {
          const kill = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
          kill.on("error", () => done(false));
          kill.on("close", (code) => done(code === 0));
        });
      }
      try { process.kill(-child.pid, "SIGKILL"); return Promise.resolve(true); }
      catch { return Promise.resolve(false); }
    };
    const finishFailure = () => {
      if (!finished && terminalError && childClosed && cleanupConfirmed) {
        finished = true; clearTimeout(cleanupDeadline); reject(new Error(terminalError));
      }
    };
    const fail = (message) => {
      if (finished || terminalError) return;
      terminalError = message; clearTimeout(timeout);
      cleanupDeadline = setTimeout(() => {
        if (finished) return;
        gatewayBlocked = true; finished = true;
        reject(new Error("工作进程清理未能确认，预览已停止接受任务，请重启预览服务。"));
      }, 5000);
      void stop().then((confirmed) => { cleanupConfirmed = confirmed; finishFailure(); });
    };
    const timeout = setTimeout(() => fail("后端查询超时，请稍后重试。"), 37000);
    child.on("error", () => fail("无法启动演示后端。"));
    child.stdout.on("data", (chunk) => {
      if (output.length + chunk.length > 262144) return fail("后端结果超出大小限制。");
      output = Buffer.concat([output, chunk]);
    });
    child.stderr.resume();
    child.on("close", (code) => {
      childClosed = true;
      if (finished) return;
      if (terminalError) return finishFailure();
      finished = true; clearTimeout(timeout);
      if (code !== 0) return reject(new Error("后端请求未完成。"));
      try { resolve(JSON.parse(output.toString("utf8"))); }
      catch { reject(new Error("后端返回格式无效。")); }
    });
    child.stdin.on("error", () => fail("后端请求通道不可用。"));
    child.stdin.end(body);
  });
}

const server = await createServer({
  root, configFile: path.join(root, "vite.config.ts"),
  server: { host: "127.0.0.1", port, strictPort: true, open: false },
  plugins: [{ name: "ai-office-local-preview", configureServer(vite) {
    vite.middlewares.use(async (req, res, next) => {
      if (req.url !== "/__ai-office") return next();
      const reply = (status, body) => { res.writeHead(status, { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "no-store" }); res.end(JSON.stringify(body)); };
      if (req.headers.host !== `127.0.0.1:${port}` || (req.headers.origin && req.headers.origin !== origin)) {
        return reply(403, { success: false, error: "仅接受当前本机演示页面的请求。" });
      }
      if (req.method !== "POST" || req.headers["content-type"] !== "application/json") return reply(405, { success: false, error: "需要 JSON POST 请求。" });
      if (pending >= 8) return reply(429, { success: false, error: "请求较多，请稍后重试。" });
      pending++;
      try {
        let body = Buffer.alloc(0);
        for await (const chunk of req) {
          if (body.length + chunk.length > 65536) { reply(413, { success: false, error: "请求过大。" }); return; }
          body = Buffer.concat([body, chunk]);
        }
        const value = JSON.parse(body.toString("utf8"));
        if (!["policies", "save_policy", "evaluate_plan", "refresh_cli"].includes(value?.op)) return reply(400, { success: false, error: "不支持的演示操作。" });
        // Serialize the complete read/CAS/write request across short-lived workers.
        const work = queue.then(() => rpc(body));
        queue = work.catch(() => {});
        reply(200, await work);
      } catch (error) { reply(400, { success: false, error: error instanceof SyntaxError ? "无效JSON。" : error.message }); }
      finally { pending--; }
    });
  } }],
});
await server.listen();
console.log(`AI Office demo: ${origin}/?ai-office=demo`);
