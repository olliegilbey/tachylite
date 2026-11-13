import { spawn, type ChildProcess } from "node:child_process";
import { join } from "node:path";

const binary = join(__dirname, "..", "target", "release", "tachylite");

export type Server = { child: ChildProcess; port: number; base: string };

/**
 * Starts a server on `dir/vault`, resolving once it prints its address. Port 0 takes a free
 * port; pass a stopped server's port to restart it on the same origin.
 */
export function serve(dir: string, port = 0): Promise<Server> {
  const child = spawn(binary, ["serve", join(dir, "vault"), join(dir, "out"), "--port", `${port}`]);
  return new Promise((resolve, reject) => {
    let log = "";
    const read = (chunk: Buffer) => {
      log += chunk.toString();
      const at = /serving (http:\/\/127\.0\.0\.1:(\d+))/.exec(log);
      if (!at) return;
      // Keep draining stderr, but stop holding it.
      child.stderr!.off("data", read).resume();
      resolve({ child, port: Number(at[2]), base: at[1] });
    };
    child.stderr!.on("data", read);
    child.on("exit", (code) => reject(new Error(`serve exited with ${code}: ${log}`)));
  });
}

/** Stops a server by the PID captured at spawn. */
export function stop({ child }: Server): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    child.on("exit", () => resolve());
    child.kill();
  });
}
