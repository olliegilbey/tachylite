import { spawn, type ChildProcess } from "node:child_process";
import { join } from "node:path";

const binary = join(__dirname, "..", "target", "release", "tachylite");

/** Starts a server on `dir/vault`, resolving once it prints its address. */
export function serve(dir: string, port: number): Promise<ChildProcess> {
  const child = spawn(binary, ["serve", join(dir, "vault"), join(dir, "out"), "--port", `${port}`]);
  return new Promise((resolve, reject) => {
    child.stderr!.on("data", (chunk: Buffer) => {
      if (chunk.toString().includes("serving http://")) resolve(child);
    });
    child.on("exit", (code) => reject(new Error(`serve exited with ${code}`)));
  });
}

/** Stops a server by the PID captured at spawn. */
export function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    child.on("exit", () => resolve());
    child.kill();
  });
}
