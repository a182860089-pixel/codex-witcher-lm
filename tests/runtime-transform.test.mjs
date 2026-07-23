import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  createExactClientAdapter,
  transformAppServerRequest,
} from "../runtime/app-server-transform.mjs";

const selection = { providerId: "acme", modelId: "acme-code" };

test("sets model and provider only for thread/start", () => {
  const start = transformAppServerRequest(
    { method: "thread/start", params: { cwd: "/repo" } },
    selection,
  );
  assert.equal(start.request.params.model, "acme-code");
  assert.equal(start.request.params.modelProvider, "acme");

  const resume = { method: "thread/resume", params: { threadId: "thread-1" } };
  const output = transformAppServerRequest(resume, selection);
  assert.equal(output.changed, false);
  assert.equal(output.request, resume);
});

test("opens model and thread provider filters without global monkey patches", () => {
  const models = transformAppServerRequest({ method: "model/list" }, selection);
  assert.equal(models.request.params.includeHidden, true);
  const threads = transformAppServerRequest({ method: "thread/list" }, selection);
  assert.deepEqual(threads.request.params.modelProviders, []);
  assert.equal(globalThis.Response.prototype.json.name, "json");
});

test("exact adapter wraps only the supplied client", async () => {
  const calls = [];
  const client = {
    request(method, params) {
      calls.push({ method, params });
      return Promise.resolve("ok");
    },
  };
  const adapter = createExactClientAdapter(client, selection);
  assert.equal(await adapter.request("thread/start", { cwd: "/repo" }), "ok");
  assert.deepEqual(calls, [
    {
      method: "thread/start",
      params: { cwd: "/repo", model: "acme-code", modelProvider: "acme" },
    },
  ]);
});

test("unknown Desktop builds are fail-closed", async () => {
  const registry = JSON.parse(
    await readFile(new URL("../compat/desktop-builds.json", import.meta.url), "utf8"),
  );
  assert.equal(registry.schemaVersion, 1);
  assert.deepEqual(registry.adapters, []);
});

