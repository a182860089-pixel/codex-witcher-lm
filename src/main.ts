import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type ReasoningEffort = "low" | "medium" | "high";
type StatusKind = "info" | "working" | "success" | "error";
type AuthKind =
  | "officialLogin"
  | "systemCredential"
  | "environmentVariable"
  | "inlineToken"
  | "commandCredential"
  | "providerManaged"
  | "unknown";

interface ModelSpec {
  id: string;
  display_name: string;
  description: string;
  context_window: number;
  default_reasoning: ReasoningEffort;
  reasoning_levels: ReasoningEffort[];
  supports_parallel_tool_calls: boolean;
  supports_images: boolean;
}

interface ProviderProfile {
  id: string;
  display_name: string;
  base_url: string;
  models: ModelSpec[];
  supports_websockets: boolean;
  credential_required: boolean;
}

interface CurrentCodexConfig {
  providerId: string;
  providerName: string;
  modelId: string | null;
  baseUrl: string | null;
  authKind: AuthKind;
  catalogPath: string | null;
}

interface DashboardState {
  configPath: string;
  configExists: boolean;
  latestBackup: string | null;
  recoveryWarnings: number;
  current: CurrentCodexConfig;
  profiles: ProviderProfile[];
  profileWarning: string | null;
}

interface FetchedModel {
  id: string;
  ownedBy: string | null;
}

interface DiscoverySummary {
  sessionId: string;
  baseUrl: string;
  models: FetchedModel[];
}

interface CredentialSessionSummary {
  sessionId: string;
  baseUrl: string;
}

const app = document.querySelector<HTMLElement>("#app");
if (!app) throw new Error("missing app root");

const browserPreview: DashboardState = {
  configPath: "~/.codex/config.toml",
  configExists: true,
  latestBackup: null,
  recoveryWarnings: 0,
  current: {
    providerId: "openai",
    providerName: "OpenAI",
    modelId: null,
    baseUrl: null,
    authKind: "officialLogin",
    catalogPath: null,
  },
  profiles: [],
  profileWarning: null,
};

let dashboard = browserPreview;
let nativeAvailable = "__TAURI_INTERNALS__" in window;
let discoveryId: string | null = null;
let discoveredModels: FetchedModel[] = [];
let selectedModels = new Set<string>();
let busy = false;

app.innerHTML = `
  <header class="app-header">
    <div class="brand">
      <div class="brand-mark" aria-hidden="true">C</div>
      <div>
        <h1>Codex 模型切换</h1>
        <p>添加 API 接入，然后为新对话一键切换模型。</p>
      </div>
    </div>
    <button id="refresh" class="button button-quiet" type="button">刷新当前配置</button>
  </header>

  <main>
    <section class="current-section" aria-labelledby="current-title">
      <div class="section-title-row">
        <div>
          <p class="section-kicker">当前使用</p>
          <h2 id="current-title">正在读取 Codex 配置…</h2>
        </div>
        <span id="current-badge" class="badge">读取中</span>
      </div>
      <div id="current-details" class="current-details"></div>
    </section>

    <section class="connections-section" aria-labelledby="connections-title">
      <div class="section-title-row">
        <div>
          <h2 id="connections-title">我的接入</h2>
          <p>保存常用接入后，可以直接选择模型并切换。</p>
        </div>
        <button id="add-connection" class="button button-primary" type="button">添加接入</button>
      </div>
      <div id="profiles" class="profile-grid"></div>
    </section>

    <section id="editor" class="editor-section" aria-labelledby="editor-title" hidden>
      <div class="section-title-row">
        <div>
          <p class="section-kicker">添加接入</p>
          <h2 id="editor-title">连接你的模型服务</h2>
          <p>通常只需要 Base URL 和 API Key。</p>
        </div>
        <button id="close-editor" class="button button-quiet" type="button">关闭</button>
      </div>

      <div class="connection-fields">
        <label>
          <span>接入名称 <small>可选</small></span>
          <input id="connection-name" autocomplete="off" placeholder="例如：我的 Coding API" />
        </label>
        <label class="field-wide">
          <span>Base URL</span>
          <input id="base-url" type="url" autocomplete="url" placeholder="https://api.example.com/v1" />
        </label>
        <label class="field-wide">
          <span>API Key</span>
          <div class="password-field">
            <input id="api-key" type="password" autocomplete="new-password" placeholder="输入 API Key" />
            <button id="toggle-key" class="field-button" type="button">显示</button>
          </div>
          <small class="field-help">Key 只会保存在这台电脑的系统密钥库中。</small>
        </label>
      </div>

      <div class="editor-actions first-step-actions">
        <button id="fetch-models" class="button button-primary" type="button">连接并获取模型</button>
      </div>

      <div id="model-step" class="model-step" hidden>
        <div class="model-toolbar">
          <div>
            <h3>选择要保留的模型</h3>
            <p>以后快速切换时，只显示你勾选的模型。</p>
          </div>
          <strong id="selected-count">已选择 0 个</strong>
        </div>
        <div class="model-controls">
          <input id="model-search" type="search" autocomplete="off" placeholder="搜索模型" />
          <button id="select-visible" class="button button-secondary" type="button">全选当前结果</button>
          <button id="clear-models" class="button button-quiet" type="button">清空</button>
        </div>
        <div id="model-list" class="model-list"></div>
        <div class="manual-model">
          <input id="manual-model" autocomplete="off" placeholder="列表里没有？手动输入模型 ID" />
          <button id="add-manual-model" class="button button-secondary" type="button">添加</button>
        </div>
        <label class="default-model-field">
          <span>保存后首先使用</span>
          <select id="default-model"></select>
        </label>
        <div class="editor-actions">
          <button id="save-only" class="button button-secondary" type="button">仅保存</button>
          <button id="save-and-switch" class="button button-primary" type="button">保存并切换</button>
        </div>
      </div>
    </section>

    <section class="footer-actions" aria-label="配置操作">
      <div class="recovery-copy">
        <strong>需要撤销？</strong>
        <span>每次切换前都会保留恢复点。</span>
      </div>
      <div class="footer-buttons">
        <button id="restore" class="button button-secondary" type="button">撤销上次切换</button>
        <button id="open-codex" class="button button-primary" type="button">我已退出，重新打开 Codex</button>
      </div>
    </section>

    <output id="status" class="status status-info" aria-live="polite">正在读取当前配置…</output>
  </main>
`;

const status = required<HTMLOutputElement>("#status");

required<HTMLButtonElement>("#refresh").addEventListener("click", refreshDashboard);
required<HTMLButtonElement>("#add-connection").addEventListener("click", openEditor);
required<HTMLButtonElement>("#close-editor").addEventListener("click", closeEditor);
required<HTMLButtonElement>("#fetch-models").addEventListener("click", fetchAvailableModels);
required<HTMLButtonElement>("#toggle-key").addEventListener("click", toggleKeyVisibility);
required<HTMLInputElement>("#base-url").addEventListener("input", invalidateDiscovery);
required<HTMLInputElement>("#base-url").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    input("#api-key").focus();
  }
});
required<HTMLInputElement>("#api-key").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    void fetchAvailableModels();
  }
});
required<HTMLInputElement>("#model-search").addEventListener("input", renderModelChoices);
required<HTMLButtonElement>("#select-visible").addEventListener("click", selectVisibleModels);
required<HTMLButtonElement>("#clear-models").addEventListener("click", () => {
  selectedModels.clear();
  renderModelChoices();
});
required<HTMLButtonElement>("#add-manual-model").addEventListener("click", addManualModel);
required<HTMLInputElement>("#manual-model").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    addManualModel();
  }
});
required<HTMLButtonElement>("#save-only").addEventListener("click", () => saveConnection(false));
required<HTMLButtonElement>("#save-and-switch").addEventListener("click", () =>
  saveConnection(true),
);
required<HTMLButtonElement>("#restore").addEventListener("click", restoreLatest);
required<HTMLButtonElement>("#open-codex").addEventListener("click", openCodex);

void refreshDashboard();

async function refreshDashboard(): Promise<void> {
  if (!nativeAvailable) {
    renderDashboard();
    setStatus("当前为界面预览；通过桌面应用打开后会自动读取真实 Codex 配置。", "info");
    return;
  }
  await run("正在读取当前 Codex 配置…", async () => {
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    if (dashboard.recoveryWarnings > 0) {
      return `已读取配置，但发现 ${dashboard.recoveryWarnings} 个未完成的恢复记录；切换前请先处理。`;
    }
    return dashboard.profileWarning
      ? "已读取当前配置，但保存的快捷接入文件需要处理；原文件没有被覆盖。"
      : "已读取当前配置。";
  }, renderConfigReadError);
}

function renderDashboard(): void {
  const currentTitle = required<HTMLElement>("#current-title");
  const currentBadge = required<HTMLElement>("#current-badge");
  const currentDetails = required<HTMLElement>("#current-details");
  const currentModel = dashboard.current.modelId ?? "自动选择";

  currentTitle.textContent = currentModel;
  currentBadge.textContent =
    dashboard.recoveryWarnings > 0
      ? "需要检查"
      : dashboard.configExists
        ? "配置已读取"
        : "使用默认配置";
  currentBadge.className = dashboard.recoveryWarnings > 0 ? "badge badge-warning" : "badge";
  currentDetails.innerHTML = `
    <div>
      <span>接入方式</span>
      <strong>${escapeHtml(dashboard.current.providerName)}</strong>
    </div>
    <div>
      <span>服务地址</span>
      <strong>${escapeHtml(dashboard.current.baseUrl ?? "OpenAI 官方服务")}</strong>
    </div>
    <div>
      <span>凭据方式</span>
      <strong>${escapeHtml(authLabel(dashboard.current.authKind))}</strong>
    </div>
  `;
  renderProfiles();
  required<HTMLButtonElement>("#restore").disabled = !dashboard.latestBackup || busy;
}

function renderProfiles(): void {
  const root = required<HTMLElement>("#profiles");
  const warning = dashboard.profileWarning
    ? `
      <div class="warning-banner">
        <strong>保存的快捷接入暂时无法读取</strong>
        <p>为保护原文件，应用不会自动覆盖它。当前 Codex 配置仍可正常查看。</p>
      </div>
    `
    : "";
  if (dashboard.profiles.length === 0) {
    root.innerHTML = `
      ${warning}
      <div class="empty-state">
        <strong>还没有保存的接入</strong>
        <p>添加第一个 Base URL 和 API Key，获取模型后即可快速切换。</p>
        <button class="button button-secondary" data-open-editor type="button">添加接入</button>
      </div>
    `;
    root
      .querySelector<HTMLButtonElement>("[data-open-editor]")
      ?.addEventListener("click", openEditor);
    return;
  }

  root.innerHTML =
    warning +
    dashboard.profiles
    .map((profile) => {
      const isCurrent = profile.id === dashboard.current.providerId;
      const selected =
        isCurrent && profile.models.some((model) => model.id === dashboard.current.modelId)
          ? dashboard.current.modelId
          : profile.models[0]?.id;
      return `
        <article class="profile-card ${isCurrent ? "profile-card-current" : ""}">
          <div class="profile-heading">
            <div>
              <h3>${escapeHtml(profile.display_name)}</h3>
              <p>${escapeHtml(readableEndpoint(profile.base_url))}</p>
            </div>
            ${isCurrent ? '<span class="badge">当前</span>' : ""}
          </div>
          <label>
            <span>切换到模型</span>
            <select data-profile-model="${escapeHtml(profile.id)}">
              ${profile.models
                .map(
                  (model) =>
                    `<option value="${escapeHtml(model.id)}" ${model.id === selected ? "selected" : ""}>${escapeHtml(model.display_name || model.id)}</option>`,
                )
                .join("")}
            </select>
          </label>
          <div class="profile-actions">
            <button class="button button-primary" data-switch-profile="${escapeHtml(profile.id)}" type="button">切换</button>
            <button class="button button-quiet danger-text" data-delete-profile="${escapeHtml(profile.id)}" type="button">移除</button>
          </div>
        </article>
      `;
    })
    .join("");

  root.querySelectorAll<HTMLButtonElement>("[data-switch-profile]").forEach((button) => {
    button.addEventListener("click", () => switchSavedProfile(button.dataset.switchProfile ?? ""));
  });
  root.querySelectorAll<HTMLButtonElement>("[data-delete-profile]").forEach((button) => {
    button.addEventListener("click", () => deleteSavedProfile(button.dataset.deleteProfile ?? ""));
  });
}

function openEditor(): void {
  required<HTMLElement>("#editor").hidden = false;
  required<HTMLInputElement>("#connection-name").focus();
  required<HTMLElement>("#editor").scrollIntoView({ behavior: "smooth", block: "start" });
}

async function closeEditor(): Promise<void> {
  if (discoveryId && nativeAvailable) {
    const sessionId = discoveryId;
    discoveryId = null;
    await invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  discoveredModels = [];
  selectedModels.clear();
  required<HTMLElement>("#editor").hidden = true;
  required<HTMLElement>("#model-step").hidden = true;
  input("#connection-name").value = "";
  input("#base-url").value = "";
  input("#api-key").value = "";
  input("#api-key").type = "password";
  required<HTMLButtonElement>("#toggle-key").textContent = "显示";
  input("#model-search").value = "";
  input("#manual-model").value = "";
}

async function fetchAvailableModels(): Promise<void> {
  const baseUrl = input("#base-url").value.trim();
  const keyInput = input("#api-key");
  const secret = keyInput.value.trim();
  if (!baseUrl || !secret) {
    setStatus("请先填写 Base URL 和 API Key。", "error");
    return;
  }

  await run("正在连接并获取模型…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用连接模型服务。");
    if (discoveryId) {
      const sessionId = discoveryId;
      discoveryId = null;
      await invoke("cancel_discovery", { sessionId }).catch(() => undefined);
    }
    let result: DiscoverySummary;
    try {
      result = await invoke<DiscoverySummary>("discover_models", {
        input: { baseUrl, secret },
      });
    } catch (error) {
      discoveredModels = [];
      selectedModels.clear();
      required<HTMLElement>("#model-step").hidden = false;
      renderModelChoices();
      throw error;
    }
    keyInput.value = "";
    discoveryId = result.sessionId;
    input("#base-url").value = result.baseUrl;
    discoveredModels = result.models;
    selectedModels.clear();
    required<HTMLElement>("#model-step").hidden = false;
    renderModelChoices();
    return result.models.length > 0
      ? `已获取 ${result.models.length} 个模型，请勾选需要保留的模型。`
      : "服务已连接，但没有返回模型。你可以手动填写模型 ID。";
  });
}

function renderModelChoices(): void {
  const query = input("#model-search").value.trim().toLocaleLowerCase();
  const visible = discoveredModels.filter(
    (model) =>
      !query ||
      model.id.toLocaleLowerCase().includes(query) ||
      model.ownedBy?.toLocaleLowerCase().includes(query),
  );
  const root = required<HTMLElement>("#model-list");
  root.innerHTML =
    visible.length === 0
      ? '<p class="model-empty">没有匹配的模型，可以在下方手动添加。</p>'
      : visible
          .map(
            (model) => `
              <label class="model-choice">
                <input type="checkbox" value="${escapeHtml(model.id)}" ${selectedModels.has(model.id) ? "checked" : ""} />
                <span>
                  <strong>${escapeHtml(model.id)}</strong>
                  ${model.ownedBy ? `<small>${escapeHtml(model.ownedBy)}</small>` : ""}
                </span>
              </label>
            `,
          )
          .join("");
  root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]').forEach((checkbox) => {
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) selectedModels.add(checkbox.value);
      else selectedModels.delete(checkbox.value);
      updateSelectionSummary();
    });
  });
  updateSelectionSummary();
}

function selectVisibleModels(): void {
  required<HTMLElement>("#model-list")
    .querySelectorAll<HTMLInputElement>('input[type="checkbox"]')
    .forEach((checkbox) => selectedModels.add(checkbox.value));
  renderModelChoices();
}

function addManualModel(): void {
  const manual = input("#manual-model");
  const id = manual.value.trim();
  if (!id) return;
  if (!/^[A-Za-z0-9._:/-]{1,128}$/u.test(id)) {
    setStatus("模型 ID 只能包含字母、数字、点、下划线、冒号、斜杠和连字符。", "error");
    return;
  }
  if (!discoveredModels.some((model) => model.id === id)) {
    discoveredModels.push({ id, ownedBy: null });
    discoveredModels.sort((left, right) => left.id.localeCompare(right.id));
  }
  selectedModels.add(id);
  manual.value = "";
  renderModelChoices();
}

function updateSelectionSummary(): void {
  required<HTMLElement>("#selected-count").textContent = `已选择 ${selectedModels.size} 个`;
  const defaultModel = required<HTMLSelectElement>("#default-model");
  const previous = defaultModel.value;
  defaultModel.innerHTML = [...selectedModels]
    .sort((left, right) => left.localeCompare(right))
    .map(
      (id) =>
        `<option value="${escapeHtml(id)}" ${id === previous ? "selected" : ""}>${escapeHtml(id)}</option>`,
    )
    .join("");
  required<HTMLButtonElement>("#save-only").disabled = selectedModels.size === 0 || busy;
  required<HTMLButtonElement>("#save-and-switch").disabled = selectedModels.size === 0 || busy;
}

async function saveConnection(activate: boolean): Promise<void> {
  const baseUrl = input("#base-url").value.trim();
  const displayName = input("#connection-name").value.trim() || defaultConnectionName(baseUrl);
  const defaultModel = required<HTMLSelectElement>("#default-model").value;
  if (!baseUrl || selectedModels.size === 0 || !defaultModel) {
    setStatus("请填写 Base URL，并至少选择一个模型。", "error");
    return;
  }
  const modelIds = [...selectedModels];

  await run(activate ? "正在保存并切换…": "正在保存接入…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用保存接入。");
    let sessionForSave = discoveryId;
    let resolvedBaseUrl = baseUrl;
    if (!sessionForSave) {
      const keyField = input("#api-key");
      const secret = keyField.value.trim();
      if (!secret) throw new Error("请输入 API Key。");
      const staged = await invoke<CredentialSessionSummary>("stage_credential", {
        input: { baseUrl, secret },
      });
      keyField.value = "";
      sessionForSave = staged.sessionId;
      resolvedBaseUrl = staged.baseUrl;
    }
    const profile = buildProfile(displayName, resolvedBaseUrl, modelIds);
    discoveryId = null;
    try {
      await invoke("save_profile", {
        input: { profile, discoveryId: sessionForSave },
      });
    } catch (error) {
      if (sessionForSave) {
        await invoke("cancel_discovery", { sessionId: sessionForSave }).catch(() => undefined);
      }
      throw error;
    }

    if (activate) {
      try {
        await invoke("apply_saved_profile", {
          profileId: profile.id,
          selectedModel: defaultModel,
        });
      } catch (error) {
        await refreshDashboard();
        throw new Error(`接入已保存，但没有切换：${friendlyError(error)}`);
      }
    }

    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    await closeEditor();
    return activate
      ? `已写入 ${displayName} / ${defaultModel}。请完全退出正在运行的 Codex，再点击“我已退出，重新打开 Codex”。`
      : `已保存 ${displayName}，现在可以从“我的接入”快速切换。`;
  });
}

async function switchSavedProfile(profileId: string): Promise<void> {
  const selector = required<HTMLSelectElement>(
    `[data-profile-model="${cssEscape(profileId)}"]`,
  );
  const profile = dashboard.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  await run(`正在切换到 ${profile.display_name}…`, async () => {
    await invoke("apply_saved_profile", {
      profileId,
      selectedModel: selector.value,
    });
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    return `已写入 ${profile.display_name} / ${selector.value}。请完全退出正在运行的 Codex，再点击“我已退出，重新打开 Codex”。`;
  });
}

async function deleteSavedProfile(profileId: string): Promise<void> {
  const profile = dashboard.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  if (
    !window.confirm(
      `从快捷列表移除“${profile.display_name}”？当前 Codex 配置和系统密钥不会改变。`,
    )
  ) {
    return;
  }
  await run("正在移除快捷接入…", async () => {
    await invoke("delete_saved_profile", { profileId });
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    return `已从快捷列表移除 ${profile.display_name}。`;
  });
}

async function restoreLatest(): Promise<void> {
  if (!dashboard.latestBackup) return;
  if (!window.confirm("撤销上一次模型切换并恢复当时的 Codex 配置？")) return;
  await run("正在恢复上一次配置…", async () => {
    await invoke("restore_latest");
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    return "已恢复上一次配置。请完全退出并重新打开 Codex。";
  });
}

async function openCodex(): Promise<void> {
  if (
    !window.confirm(
      "请先完全退出正在运行的 Codex。确认已经退出，并重新打开 Codex 吗？",
    )
  ) {
    return;
  }
  await run("正在打开 Codex…", async () => {
    await invoke("open_codex");
    return "已请求系统打开 Codex。";
  });
}

function buildProfile(
  displayName: string,
  baseUrl: string,
  modelIds: string[],
): ProviderProfile {
  return {
    id: providerId(displayName, baseUrl),
    display_name: displayName,
    base_url: baseUrl,
    models: modelIds
      .sort((left, right) => left.localeCompare(right))
      .map((id) => ({
        id,
        display_name: id,
        description: "",
        context_window: 128_000,
        default_reasoning: "medium",
        reasoning_levels: ["low", "medium", "high"],
        supports_parallel_tool_calls: true,
        supports_images: false,
      })),
    supports_websockets: false,
    credential_required: true,
  };
}

function providerId(name: string, baseUrl: string): string {
  const slug =
    name
      .toLocaleLowerCase()
      .normalize("NFKD")
      .replace(/[^a-z0-9]+/gu, "-")
      .replace(/^-+|-+$/gu, "")
      .slice(0, 36) || "connection";
  return `cps-${slug}-${shortHash(baseUrl)}`;
}

function shortHash(value: string): string {
  let hash = 2_166_136_261;
  for (const character of value.trim().replace(/\/+$/u, "").toLocaleLowerCase()) {
    hash ^= character.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 16_777_619);
  }
  return (hash >>> 0).toString(36).slice(0, 7);
}

function defaultConnectionName(baseUrl: string): string {
  try {
    return new URL(baseUrl).hostname;
  } catch {
    return "我的 API";
  }
}

function readableEndpoint(baseUrl: string): string {
  try {
    const parsed = new URL(baseUrl);
    return `${parsed.host}${parsed.pathname.replace(/\/+$/u, "")}`;
  } catch {
    return baseUrl;
  }
}

function authLabel(kind: AuthKind): string {
  return (
    {
      officialLogin: "OpenAI 登录 / API Key",
      systemCredential: "系统密钥库",
      environmentVariable: "环境变量",
      inlineToken: "配置文件中的令牌",
      commandCredential: "认证命令",
      providerManaged: "现有 Codex 认证",
      unknown: "未识别",
    } satisfies Record<AuthKind, string>
  )[kind];
}

function toggleKeyVisibility(): void {
  const key = input("#api-key");
  const button = required<HTMLButtonElement>("#toggle-key");
  key.type = key.type === "password" ? "text" : "password";
  button.textContent = key.type === "password" ? "显示" : "隐藏";
}

async function run(
  message: string,
  action: () => Promise<string>,
  onError?: (error: unknown) => void,
): Promise<void> {
  setBusy(true);
  setStatus(message, "working");
  try {
    setStatus(await action(), "success");
  } catch (error) {
    onError?.(error);
    setStatus(friendlyError(error), "error");
  } finally {
    setBusy(false);
  }
}

function setBusy(value: boolean): void {
  busy = value;
  document.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
    button.disabled = value;
  });
  document
    .querySelectorAll<HTMLInputElement | HTMLSelectElement>("input, select")
    .forEach((control) => {
      control.disabled = value;
    });
  if (!value) {
    required<HTMLButtonElement>("#restore").disabled = !dashboard.latestBackup;
    updateSelectionSummary();
  }
}

function setStatus(message: string, kind: StatusKind): void {
  status.value = message;
  status.className = `status status-${kind}`;
}

function friendlyError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (message.includes("HTTP 401") || message.includes("HTTP 403")) {
    return "API Key 无效，或没有读取模型的权限。";
  }
  if (message.includes("HTTP 404") || message.includes("HTTP 405")) {
    return "这个地址没有提供模型列表。请检查 Base URL，或手动填写模型 ID。";
  }
  if (message.includes("timed out")) {
    return "连接超时，请检查网络和 Base URL。";
  }
  if (message.includes("too large")) {
    return "模型列表过大，已停止读取。";
  }
  if (
    message.includes("changed after") ||
    message.includes("no longer matches") ||
    message.includes("changed immediately")
  ) {
    return "Codex 配置刚刚被其他程序修改。为避免覆盖，本次没有切换。";
  }
  if (
    message.includes("non-loopback provider URLs must use HTTPS") ||
    message.includes("provider URL scheme must be HTTPS")
  ) {
    return "远程 Base URL 必须使用 HTTPS；只有本机 localhost 地址可以使用 HTTP。";
  }
  if (
    message.includes("relative URL without a base") ||
    message.includes("invalid port number") ||
    message.includes("base URL must not contain")
  ) {
    return "Base URL 格式不正确。请填写完整地址，例如 https://api.example.com/v1。";
  }
  if (message.includes("could not connect to the model endpoint")) {
    return "无法连接模型服务。请检查网络和 Base URL；也可以在下方手动填写模型 ID。";
  }
  if (message.includes("unsupported response")) {
    return "服务返回的模型列表格式无法识别。你仍可在下方手动填写模型 ID。";
  }
  if (message.includes("discovery session expired")) {
    return "临时 API Key 已过期，请重新输入 Key 并获取模型。";
  }
  if (message.includes("Base URL changed after models were fetched")) {
    return "Base URL 已改变，请重新输入 API Key 并获取模型。";
  }
  if (message.includes("enter the API Key") || message.includes("API Key is required")) {
    return "请输入 API Key 后再继续。";
  }
  if (message.toLocaleLowerCase().includes("credential")) {
    return "系统密钥库操作失败；本次没有修改 Codex 配置。";
  }
  if (message.includes("saved connections")) {
    return "无法保存快捷接入；原有快捷接入和 Codex 配置没有被覆盖。";
  }
  if (
    message.includes("could not read the Codex configuration") ||
    message.includes("Codex configuration is not valid UTF-8") ||
    message.includes("unsafe configuration path")
  ) {
    return "无法安全读取当前 Codex 配置。请检查 config.toml 是否为正常的 UTF-8 文件。";
  }
  if (message === "[object Object]" || !message.trim()) {
    return "操作失败；Codex 配置没有被修改。";
  }
  return message;
}

function renderConfigReadError(): void {
  required<HTMLElement>("#current-title").textContent = "无法读取当前配置";
  const badge = required<HTMLElement>("#current-badge");
  badge.textContent = "读取失败";
  badge.className = "badge badge-warning";
  required<HTMLElement>("#current-details").innerHTML = `
    <div>
      <span>配置位置</span>
      <strong>${escapeHtml(dashboard.configPath)}</strong>
    </div>
    <div>
      <span>下一步</span>
      <strong>检查配置文件后点击“刷新当前配置”</strong>
    </div>
  `;
}

function invalidateDiscovery(): void {
  if (!discoveryId) return;
  const sessionId = discoveryId;
  discoveryId = null;
  discoveredModels = [];
  selectedModels.clear();
  required<HTMLElement>("#model-step").hidden = true;
  if (nativeAvailable) {
    void invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  setStatus("Base URL 已改变，请重新输入 API Key 并获取模型。", "info");
}

function cssEscape(value: string): string {
  return value.replaceAll("\\", "\\\\").replaceAll('"', '\\"');
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function input(selector: string): HTMLInputElement {
  return required<HTMLInputElement>(selector);
}

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`missing element ${selector}`);
  return element;
}
