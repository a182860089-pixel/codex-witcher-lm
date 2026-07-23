const SUPPORTED_METHODS = new Set(["model/list", "thread/list", "thread/start"]);

export function transformAppServerRequest(request, selection) {
  validateSelection(selection);
  if (!isRecord(request) || typeof request.method !== "string") {
    return { request, changed: false };
  }
  if (!SUPPORTED_METHODS.has(request.method)) {
    return { request, changed: false };
  }

  const cloned = structuredClone(request);
  if (cloned.params === undefined) cloned.params = {};
  if (!isRecord(cloned.params)) {
    throw new TypeError("App Server request params must be an object");
  }

  if (cloned.method === "model/list") {
    cloned.params.includeHidden = true;
  } else if (cloned.method === "thread/list") {
    cloned.params.modelProviders = [];
  } else if (cloned.method === "thread/start") {
    cloned.params.model = selection.modelId;
    cloned.params.modelProvider = selection.providerId;
  }
  return { request: cloned, changed: true };
}

export function createExactClientAdapter(client, selection) {
  if (!isRecord(client) || typeof client.request !== "function") {
    throw new TypeError("reviewed adapter requires an App Server client.request function");
  }

  return Object.freeze({
    request(method, params) {
      const transformed = transformAppServerRequest({ method, params }, selection);
      return client.request.call(client, transformed.request.method, transformed.request.params);
    },
  });
}

function validateSelection(selection) {
  if (
    !isRecord(selection) ||
    !isIdentifier(selection.providerId) ||
    !isIdentifier(selection.modelId)
  ) {
    throw new TypeError("invalid provider/model selection");
  }
}

function isIdentifier(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 128 &&
    !/[\u0000-\u001f\u007f]/u.test(value)
  );
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

