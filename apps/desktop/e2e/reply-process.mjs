import { spawn } from "node:child_process";

import { DEGENERATE_RUN_LENGTH, scanRepetition } from "./reply-failure.mjs";

// A separate process group lets a timeout or degenerate stream stop descendants too.
export function runReply(command, args, { env, timeout }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
    const result = { stdout: "", stderr: "" };
    const runs = {};
    let bytes = 0;
    let stopped = false;
    const timer = setTimeout(() => stop({
      error: Object.assign(new Error(`spawn ${command} ETIMEDOUT`), { code: "ETIMEDOUT" }),
    }), timeout);

    function stop(failure) {
      if (stopped) return;
      stopped = true;
      Object.assign(result, failure);
      if (!child.pid) return;
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if (error.code !== "ESRCH") {
          clearTimeout(timer);
          reject(error);
        }
      }
    }

    for (const name of ["stdout", "stderr"]) {
      child[name].setEncoding("utf8");
      child[name].on("data", (text) => {
        result[name] += text;
        if (stopped) return;
        runs[name] = scanRepetition(runs[name], text);
        if (runs[name].length >= DEGENERATE_RUN_LENGTH) return stop({ degenerate: runs[name] });
        bytes += Buffer.byteLength(text);
        if (bytes > 64 * 1024 * 1024) {
          stop({ error: Object.assign(new Error(`spawn ${command} ENOBUFS`), { code: "ENOBUFS" }) });
        }
      });
      child[name].on("error", (error) => stop({ error }));
    }
    child.once("error", (error) => stop({ error }));
    child.once("close", (status, signal) => {
      clearTimeout(timer);
      resolve({ ...result, status, signal });
    });
  });
}
