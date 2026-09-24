// Single-instance lock: an exclusively created file holding our PID. A lock
// left behind by a crashed process (PID no longer alive) is taken over.
import fs from "node:fs";

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}

export function acquireLock(file) {
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = fs.openSync(file, "wx", 0o600);
      fs.writeSync(fd, String(process.pid));
      fs.closeSync(fd);
      const release = () => {
        try {
          if (fs.readFileSync(file, "utf8").trim() === String(process.pid)) fs.unlinkSync(file);
        } catch {
          /* already gone */
        }
      };
      return { ok: true, release };
    } catch (e) {
      if (e.code !== "EEXIST") throw e;
      const pid = Number.parseInt(fs.readFileSync(file, "utf8").trim(), 10);
      if (Number.isInteger(pid) && pid > 0 && pid !== process.pid && alive(pid)) {
        return { ok: false, pid };
      }
      fs.rmSync(file, { force: true });
    }
  }
  return { ok: false, pid: null };
}
