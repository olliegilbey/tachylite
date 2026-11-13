import { cpSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { serve, stop } from "./server";

// Each run gets its own directory and a server on a free port, so runs on one host never meet.
export default async function () {
  mkdirSync(join(__dirname, ".tmp"), { recursive: true });
  const dir = mkdtempSync(join(__dirname, ".tmp", "run-"));
  cpSync(join(__dirname, "..", "tests", "fixtures", "vault"), join(dir, "vault"), { recursive: true });
  const server = await serve(dir);
  process.env.TACHYLITE_RUN = dir;
  process.env.TACHYLITE_BASE = server.base;
  return async () => {
    await stop(server);
    rmSync(dir, { recursive: true, force: true });
  };
}
