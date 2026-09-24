import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, send } from "../transport";

afterEach(() => vi.unstubAllGlobals());

describe("transport", () => {
  it("returns data from the development bridge", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true, data: { x: 1 } })));
    vi.stubGlobal("fetch", fetchMock);
    await expect(send("system.ping", "tok", { a: 1 })).resolves.toEqual({ x: 1 });
    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(body).toEqual({ cmd: "system.ping", token: "tok", args: { a: 1 } });
  });

  it("surfaces backend errors as ApiError with their code", async () => {
    const error = {
      code: "approval_required",
      message: "Manager approval required",
      data_changed: false,
      retryable: false,
    };
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: false, error }))));
    const e = await send("pos.finalize", "tok").catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect((e as ApiError).code).toBe("approval_required");
  });

  it("reports network failure as retryable with no data change", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    const e = (await send("pos.finalize", "tok").catch((x: unknown) => x)) as ApiError;
    expect(e.code).toBe("transport");
    expect(e.retryable).toBe(true);
    expect(e.data_changed).toBe(false);
  });
});
