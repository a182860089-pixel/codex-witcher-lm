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

type SwitchMode = "localProxy" | "directConfig";

interface LocalProxyStatus {
  enabled: boolean;
  running: boolean;
  recoveryRequired: boolean;
  manualRecoveryRequired: boolean;
  currentProfileId: string | null;
  currentModelId: string | null;
  requiresCodexRestart: boolean;
  lastError: string | null;
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

const stoppedProxy: LocalProxyStatus = {
  enabled: false,
  running: false,
  recoveryRequired: false,
  manualRecoveryRequired: false,
  currentProfileId: null,
  currentModelId: null,
  requiresCodexRestart: false,
  lastError: null,
};

let dashboard = browserPreview;
let nativeAvailable = "__TAURI_INTERNALS__" in window;
let proxyApiAvailable = true;
let localProxy = stoppedProxy;
let switchMode: SwitchMode = "localProxy";
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
        <p>选择接入和模型，下一次发送即可使用。</p>
      </div>
    </div>
    <button id="refresh" class="button button-quiet" type="button">刷新状态</button>
  </header>

  <main>
    <section class="current-section" aria-labelledby="current-title">
      <div class="section-title-row">
        <div>
          <p id="current-kicker" class="section-kicker">当前选择</p>
          <h2 id="current-title">正在读取…</h2>
        </div>
        <span id="current-badge" class="badge">读取中</span>
      </div>
      <div id="current-details" class="current-details"></div>
    </section>

    <section class="switch-mode-section" aria-labelledby="switch-mode-title">
      <div class="section-title-row">
        <div>
          <div class="title-with-recommendation">
            <h2 id="switch-mode-title">选择切换方式</h2>
            <span class="recommendation">推荐</span>
          </div>
          <p>快速切换不会反复改动 Codex 设置，更适合日常使用。</p>
        </div>
        <div id="proxy-health" class="proxy-health" role="status">
          <span class="health-dot" aria-hidden="true"></span>
          <strong>正在检查</strong>
        </div>
      </div>

      <div class="mode-options" role="group" aria-label="切换方式">
        <button id="mode-local-proxy" class="mode-option mode-option-active" type="button" aria-pressed="true">
          <span class="mode-option-heading">
            <strong>快速切换</strong>
            <span class="mini-badge">推荐</span>
          </span>
          <span>由本机安全转发请求，切换后从下一次发送开始使用。</span>
        </button>
        <button id="mode-direct-config" class="mode-option" type="button" aria-pressed="false">
          <span class="mode-option-heading">
            <strong>直接配置</strong>
            <span class="mode-label">兼容</span>
          </span>
          <span>遇到兼容问题时使用；每次切换后需要重新打开 Codex。</span>
        </button>
      </div>

      <div class="mode-summary">
        <div>
          <strong id="mode-summary-title">快速切换尚未开启</strong>
          <p id="mode-summary-copy">在下方选择一个接入和模型即可开启。</p>
        </div>
        <button id="stop-proxy" class="button button-secondary" type="button" hidden>关闭快速切换</button>
      </div>
    </section>

    <section class="connections-section" aria-labelledby="connections-title">
      <div class="section-title-row">
        <div>
          <h2 id="connections-title">我的接入</h2>
          <p id="connections-help">选择模型后，下一次发送即可使用。</p>
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
          <button id="save-and-switch" class="button button-primary" type="button">保存并使用</button>
        </div>
      </div>
    </section>

    <section class="footer-actions" aria-label="配置操作">
      <div class="recovery-copy">
        <strong id="footer-title">下一次发送生效</strong>
        <span id="footer-copy">正在生成的回复不会被中断。</span>
      </div>
      <div class="footer-buttons">
        <button id="restore" class="button button-secondary" type="button">撤销上次配置更改</button>
        <button id="open-codex" class="button button-primary" type="button">重新打开 Codex</button>
      </div>
    </section>

    <output id="status" class="status status-info" aria-live="polite">正在读取当前状态…</output>
  </main>
`;

const status = required<HTMLOutputElement>("#status");

required<HTMLButtonElement>("#refresh").addEventListener("click", refreshDashboard);
required<HTMLButtonElement>("#mode-local-proxy").addEventListener("click", () =>
  selectSwitchMode("localProxy"),
);
required<HTMLButtonElement>("#mode-direct-config").addEventListener("click", () =>
  selectSwitchMode("directConfig"),
);
required<HTMLButtonElement>("#stop-proxy").addEventListener("click", disableLocalProxy);
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
    setStatus("通过桌面应用打开后，会自动读取当前接入和模型。", "info");
    return;
  }
  await run("正在读取当前状态…", async () => {
    dashboard = await invoke<DashboardState>("inspect_state");
    await refreshProxyStatus();
    renderDashboard();
    if (dashboard.recoveryWarnings > 0) {
      return "检测到无法自动完成的恢复记录。应用已停止配置写入，请人工检查恢复文件后刷新。";
    }
    if (dashboard.profileWarning) {
      return "保存的接入暂时无法读取；原有内容没有被覆盖。";
    }
    if (!proxyApiAvailable) {
      return "已读取当前设置。快速切换暂不可用，可以选择直接配置。";
    }
    if (localProxy.manualRecoveryRequired) {
      return "快速切换的配置或恢复记录无法安全读取；应用不会覆盖文件，请人工处理后刷新。";
    }
    if (localProxy.recoveryRequired) return "快速切换需要修复，请关闭并恢复原设置。";
    return localProxy.running ? "快速切换正在运行。" : "已读取当前设置。";
  }, renderConfigReadError);
}

function renderDashboard(): void {
  const proxyProfile = dashboard.profiles.find(
    (profile) => profile.id === localProxy.currentProfileId,
  );
  const proxyIsActive =
    localProxy.enabled && localProxy.running && !localProxy.recoveryRequired;
  const currentConnection = proxyIsActive
    ? (proxyProfile?.display_name ?? "已保存的接入")
    : dashboard.current.providerName;
  const currentModel = proxyIsActive
    ? (localProxy.currentModelId ?? "自动选择")
    : (dashboard.current.modelId ?? "自动选择");
  const currentEndpoint = proxyIsActive
    ? (proxyProfile?.base_url ?? "已保存的模型服务")
    : (dashboard.current.baseUrl ?? "OpenAI 官方服务");
  const currentAuth = proxyIsActive
    ? "系统密钥库 · 本机转发"
    : authLabel(dashboard.current.authKind);
  const currentKicker = required<HTMLElement>("#current-kicker");
  const currentTitle = required<HTMLElement>("#current-title");
  const currentBadge = required<HTMLElement>("#current-badge");
  const currentDetails = required<HTMLElement>("#current-details");

  currentTitle.textContent = currentModel;
  currentKicker.textContent = proxyIsActive ? "下一次发送将使用" : "Codex 当前使用";
  if (dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired) {
    currentBadge.textContent = "需人工处理";
    currentBadge.className = "badge badge-warning";
  } else if (localProxy.recoveryRequired) {
    currentBadge.textContent = "可安全恢复";
    currentBadge.className = "badge badge-warning";
  } else if (proxyIsActive) {
    currentBadge.textContent = "快速切换已开启";
    currentBadge.className = "badge";
  } else {
    currentBadge.textContent = dashboard.configExists ? "直接配置" : "默认设置";
    currentBadge.className = "badge badge-neutral";
  }
  currentDetails.innerHTML = `
    <div>
      <span>接入方式</span>
      <strong>${escapeHtml(currentConnection)}</strong>
    </div>
    <div>
      <span>当前模型</span>
      <strong>${escapeHtml(currentModel)}</strong>
    </div>
    <div>
      <span>服务地址</span>
      <strong>${escapeHtml(readableEndpoint(currentEndpoint))}</strong>
    </div>
    <div>
      <span>凭据方式</span>
      <strong>${escapeHtml(currentAuth)}</strong>
    </div>
  `;
  renderSwitchExperience();
  renderProfiles();
  required<HTMLButtonElement>("#restore").disabled =
    !dashboard.latestBackup ||
    dashboard.recoveryWarnings > 0 ||
    localProxy.enabled ||
    localProxy.recoveryRequired ||
    busy;
}

function renderSwitchExperience(): void {
  const localButton = required<HTMLButtonElement>("#mode-local-proxy");
  const directButton = required<HTMLButtonElement>("#mode-direct-config");
  const health = required<HTMLElement>("#proxy-health");
  const summaryTitle = required<HTMLElement>("#mode-summary-title");
  const summaryCopy = required<HTMLElement>("#mode-summary-copy");
  const stopButton = required<HTMLButtonElement>("#stop-proxy");
  const connectionsHelp = required<HTMLElement>("#connections-help");
  const saveAndSwitch = required<HTMLButtonElement>("#save-and-switch");
  const footerTitle = required<HTMLElement>("#footer-title");
  const footerCopy = required<HTMLElement>("#footer-copy");
  const openCodexButton = required<HTMLButtonElement>("#open-codex");
  const manualRecoveryBlocked =
    dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired;

  localButton.classList.toggle("mode-option-active", switchMode === "localProxy");
  localButton.setAttribute("aria-pressed", String(switchMode === "localProxy"));
  directButton.classList.toggle("mode-option-active", switchMode === "directConfig");
  directButton.setAttribute("aria-pressed", String(switchMode === "directConfig"));

  health.className = "proxy-health";
  if (!proxyApiAvailable) {
    health.classList.add("proxy-health-muted");
    health.querySelector("strong")!.textContent = "暂不可用";
  } else if (manualRecoveryBlocked) {
    health.classList.add("proxy-health-warning");
    health.querySelector("strong")!.textContent = "需人工处理";
  } else if (localProxy.recoveryRequired) {
    health.classList.add("proxy-health-warning");
    health.querySelector("strong")!.textContent = "需要修复";
  } else if (localProxy.running) {
    health.classList.add("proxy-health-running");
    health.querySelector("strong")!.textContent = "运行中";
  } else if (localProxy.enabled || localProxy.lastError) {
    health.classList.add("proxy-health-warning");
    health.querySelector("strong")!.textContent = "需要重新开启";
  } else {
    health.classList.add("proxy-health-muted");
    health.querySelector("strong")!.textContent = "尚未开启";
  }

  if (manualRecoveryBlocked) {
    summaryTitle.textContent = "需要人工检查";
    summaryCopy.textContent =
      "Codex 配置或快速切换恢复记录无法安全读取；应用已停止转发，也不会覆盖这些文件。";
    connectionsHelp.textContent = "请先人工修复 Codex 配置或恢复记录，然后点击“刷新状态”。";
    saveAndSwitch.textContent = "修复后可继续";
    footerTitle.textContent = "当前不会写入配置";
    footerCopy.textContent = "确认文件恢复正常后刷新，即可重新使用切换功能。";
    openCodexButton.hidden = true;
  } else if (switchMode === "directConfig") {
    summaryTitle.textContent = "直接配置（兼容）";
    summaryCopy.textContent = localProxy.enabled || localProxy.recoveryRequired
      ? "切换时会先安全关闭快速切换，再更新 Codex 设置。"
      : "适合快速切换不可用的情况；切换后需要重新打开 Codex。";
    connectionsHelp.textContent = "选择模型后会更新 Codex 设置，完成后需要重新打开 Codex。";
    saveAndSwitch.textContent = "保存并写入配置";
    footerTitle.textContent = "兼容方式需要重新打开";
    footerCopy.textContent = "每次更改后，重新打开 Codex 即可生效。";
    openCodexButton.hidden = false;
  } else if (!proxyApiAvailable) {
    summaryTitle.textContent = "快速切换暂不可用";
    summaryCopy.textContent = "你仍可选择“直接配置”完成模型切换。";
    connectionsHelp.textContent = "快速切换暂不可用，请先选择上方的直接配置。";
    saveAndSwitch.textContent = "保存并使用";
    footerTitle.textContent = "可以使用兼容方式";
    footerCopy.textContent = "选择“直接配置”后仍可安全切换。";
    openCodexButton.hidden = true;
  } else if (localProxy.recoveryRequired) {
    summaryTitle.textContent = "快速切换需要修复";
    summaryCopy.textContent = "请先关闭快速切换并恢复原设置，再重新选择接入和模型。";
    connectionsHelp.textContent = "修复完成前不会发送新的本地转发请求。";
    saveAndSwitch.textContent = "请先完成修复";
    footerTitle.textContent = "原设置受到保护";
    footerCopy.textContent = "点击“关闭快速切换”执行受验证的恢复。";
    openCodexButton.hidden = true;
  } else if (localProxy.running) {
    const profile = dashboard.profiles.find(
      (item) => item.id === localProxy.currentProfileId,
    );
    summaryTitle.textContent = "快速切换已开启";
    summaryCopy.textContent = localProxy.requiresCodexRestart
      ? "首次设置已完成。重新打开一次 Codex 后，今后的切换无需重复重启。"
      : `${profile?.display_name ?? "当前接入"} · ${localProxy.currentModelId ?? "自动选择"}，下一次发送生效。`;
    connectionsHelp.textContent = "选择模型后立即准备好；正在生成的回复不会被中断。";
    saveAndSwitch.textContent = "保存并使用";
    footerTitle.textContent = localProxy.requiresCodexRestart
      ? "首次开启需要重新打开一次"
      : "下一次发送生效";
    footerCopy.textContent = localProxy.requiresCodexRestart
      ? "完成这一次后，今后切换模型无需重复重启。"
      : "正在生成的回复不会被中断。";
    openCodexButton.hidden = !localProxy.requiresCodexRestart;
  } else if (localProxy.enabled) {
    summaryTitle.textContent = "快速切换需要重新开启";
    summaryCopy.textContent = "选择一个接入和模型即可安全修复；也可以关闭并恢复原设置。";
    connectionsHelp.textContent = "重新选择接入和模型后，应用会检查并修复快速切换。";
    saveAndSwitch.textContent = "保存并重新开启";
    footerTitle.textContent = "当前没有转发请求";
    footerCopy.textContent = "完成重新开启前，应用不会假装切换已经生效。";
    openCodexButton.hidden = true;
  } else {
    summaryTitle.textContent = "快速切换尚未开启";
    summaryCopy.textContent = "在下方选择一个接入和模型，即可开启并使用。";
    connectionsHelp.textContent = "选择模型后开启快速切换，下一次发送即可使用。";
    saveAndSwitch.textContent = "保存并使用";
    footerTitle.textContent = "首次开启只需设置一次";
    footerCopy.textContent = "之后切换接入或模型，无需反复重新打开 Codex。";
    openCodexButton.hidden = true;
  }

  stopButton.hidden =
    localProxy.manualRecoveryRequired ||
    (!localProxy.enabled && !localProxy.recoveryRequired);
  stopButton.disabled = busy;
  if (
    manualRecoveryBlocked ||
    (switchMode === "localProxy" && localProxy.recoveryRequired)
  ) {
    saveAndSwitch.disabled = true;
  }
}

function renderProfiles(): void {
  const root = required<HTMLElement>("#profiles");
  const manualRecoveryBlocked =
    dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired;
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
        <p>添加 Base URL 和 API Key，选择模型后即可开始使用。</p>
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
      const proxyIsActive =
        localProxy.enabled && localProxy.running && !localProxy.recoveryRequired;
      const isCurrent = proxyIsActive
        ? profile.id === localProxy.currentProfileId
        : profile.id === dashboard.current.providerId;
      const activeModel = proxyIsActive
        ? localProxy.currentModelId
        : dashboard.current.modelId;
      const selected =
        isCurrent && profile.models.some((model) => model.id === activeModel)
          ? activeModel
          : profile.models[0]?.id;
      const actionLabel =
        manualRecoveryBlocked
          ? "请先人工处理"
          : switchMode === "directConfig"
          ? "写入配置"
          : localProxy.recoveryRequired
            ? "请先完成修复"
          : localProxy.running && !localProxy.recoveryRequired
            ? "下一次发送生效"
            : "开启并使用";
      return `
        <article class="profile-card ${isCurrent ? "profile-card-current" : ""}">
          <div class="profile-heading">
            <div>
              <h3>${escapeHtml(profile.display_name)}</h3>
              <p>${escapeHtml(readableEndpoint(profile.base_url))}</p>
            </div>
            ${isCurrent ? `<span class="badge">${proxyIsActive ? "下一次" : "当前"}</span>` : ""}
          </div>
          <label>
            <span>选择模型</span>
            <select data-profile-model="${escapeHtml(profile.id)}">
              ${profile.models
                .map(
                  (model) =>
                    `<option value="${escapeHtml(model.id)}" ${model.id === selected ? "selected" : ""}>${escapeHtml(model.display_name || model.id)}</option>`,
                )
                .join("")}
            </select>
          </label>
          <p class="profile-effect">${
            switchMode === "localProxy"
              ? "选择后从下一次发送开始使用"
              : "选择后将更新 Codex 设置"
          }</p>
          <div class="profile-actions">
            <button class="button button-primary" data-switch-profile="${escapeHtml(profile.id)}" type="button" ${manualRecoveryBlocked || (switchMode === "localProxy" && localProxy.recoveryRequired) ? "disabled" : ""}>${actionLabel}</button>
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

function selectSwitchMode(mode: SwitchMode): void {
  switchMode = mode;
  renderDashboard();
  updateSelectionSummary();
  if (dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired) {
    setStatus(
      "配置或恢复记录需要人工处理；应用不会在修复前写入 Codex 配置。",
      "error",
    );
    return;
  }
  setStatus(
    mode === "localProxy"
      ? "已选择快速切换。选择一个接入和模型即可继续。"
      : "已选择直接配置。切换后需要重新打开 Codex。",
    "info",
  );
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
  required<HTMLButtonElement>("#save-and-switch").disabled =
    selectedModels.size === 0 ||
    busy ||
    dashboard.recoveryWarnings > 0 ||
    localProxy.manualRecoveryRequired ||
    (switchMode === "localProxy" && localProxy.recoveryRequired);
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

  const mode = switchMode;
  await run(activate ? "正在保存并切换…" : "正在保存接入…", async () => {
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
    const connectionChanged = activate && isCrossConnectionSwitch(profile.id);
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
        await activateProfile(profile.id, defaultModel);
      } catch (error) {
        await refreshDashboard();
        const action = mode === "directConfig" ? "配置切换" : "快速切换";
        throw new Error(`接入已保存；${action}未能确认：${friendlyError(error)}`);
      }
    }

    if (activate) {
      await refreshAfterMutation(mode, profile.id, defaultModel);
    } else {
      try {
        dashboard = await invoke<DashboardState>("inspect_state");
        await refreshProxyStatus();
        renderDashboard();
      } catch {
        throw new Error("接入已保存，但列表刷新失败；请点击“刷新状态”确认。");
      }
    }
    await closeEditor();
    if (!activate) {
      return `已保存 ${displayName}，现在可以从“我的接入”中选择。`;
    }
    if (mode === "directConfig") {
      return `已写入 ${displayName} / ${defaultModel}。请重新打开 Codex 以生效。${crossConnectionAdvice(connectionChanged)}`;
    }
    return proxyCompletionMessage(localProxy.requiresCodexRestart
      ? `已准备 ${displayName} / ${defaultModel}。首次开启需要重新打开一次 Codex。${crossConnectionAdvice(connectionChanged)}`
      : `已切换到 ${displayName} / ${defaultModel}，下一次发送生效。${crossConnectionAdvice(connectionChanged)}`);
  });
}

async function switchSavedProfile(profileId: string): Promise<void> {
  const selector = required<HTMLSelectElement>(
    `[data-profile-model="${cssEscape(profileId)}"]`,
  );
  const profile = dashboard.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  const selectedModel = selector.value;
  const mode = switchMode;
  const connectionChanged = isCrossConnectionSwitch(profileId);
  await run(
    mode === "localProxy"
      ? `正在准备 ${profile.display_name}…`
      : `正在更新为 ${profile.display_name}…`,
    async () => {
    await activateProfile(profileId, selectedModel);
    await refreshAfterMutation(mode, profileId, selectedModel);
    if (mode === "directConfig") {
      return `已写入 ${profile.display_name} / ${selectedModel}。请重新打开 Codex 以生效。${crossConnectionAdvice(connectionChanged)}`;
    }
    return proxyCompletionMessage(localProxy.requiresCodexRestart
      ? `已准备 ${profile.display_name} / ${selectedModel}。首次开启需要重新打开一次 Codex。${crossConnectionAdvice(connectionChanged)}`
      : `已切换到 ${profile.display_name} / ${selectedModel}，下一次发送生效。${crossConnectionAdvice(connectionChanged)}`);
  },
  );
}

function isCrossConnectionSwitch(targetProfileId: string): boolean {
  const currentProfileId =
    localProxy.enabled && !localProxy.recoveryRequired && localProxy.currentProfileId
      ? localProxy.currentProfileId
      : dashboard.current.providerId;
  return currentProfileId.length > 0 && currentProfileId !== targetProfileId;
}

function crossConnectionAdvice(changed: boolean): string {
  return changed ? " 为保证上下文兼容，建议新建对话。" : "";
}

function proxyCompletionMessage(message: string): string {
  return message;
}

function requireActiveProxySelection(profileId: string, modelId: string): void {
  if (!proxyApiAvailable) {
    throw new Error("切换指令已提交，但暂时无法确认本地转发状态；请点击“刷新状态”。");
  }
  if (localProxy.recoveryRequired) {
    throw new Error("本地转发需要修复；请先关闭快速切换并恢复原设置。");
  }
  if (!localProxy.enabled || !localProxy.running) {
    throw new Error(
      localProxy.lastError
        ? `本地转发未能保持运行：${localProxy.lastError}`
        : "本地转发未能保持运行，请点击“刷新状态”后重试。",
    );
  }
  if (
    localProxy.currentProfileId !== profileId ||
    localProxy.currentModelId !== modelId
  ) {
    throw new Error("本地转发状态与所选接入或模型不一致；本次不报告切换成功。");
  }
}

async function refreshAfterMutation(
  mode: SwitchMode,
  profileId: string,
  modelId: string,
): Promise<void> {
  try {
    dashboard = await invoke<DashboardState>("inspect_state");
  } catch {
    throw new Error("切换指令已完成，但无法刷新 Codex 当前设置；请点击“刷新状态”确认。");
  }
  await refreshProxyStatus();
  renderDashboard();
  if (dashboard.recoveryWarnings > 0) {
    throw new Error(
      "设置已变更，但恢复记录无法自动完成；应用已停止继续写入，请人工检查后刷新。",
    );
  }
  if (mode === "localProxy") {
    requireActiveProxySelection(profileId, modelId);
  } else {
    const profile = dashboard.profiles.find((item) => item.id === profileId);
    const expectedAuth: AuthKind = profile?.credential_required
      ? "systemCredential"
      : "providerManaged";
    if (
      !profile ||
      dashboard.current.providerId !== profileId ||
      dashboard.current.modelId !== modelId ||
      normalizedEndpoint(dashboard.current.baseUrl) !==
        normalizedEndpoint(profile.base_url) ||
      dashboard.current.authKind !== expectedAuth
    ) {
      throw new Error(
        "Codex 配置已写入，但接入地址、模型或凭据方式被其他程序立即改变；请刷新状态后重试。",
      );
    }
  }
}

async function activateProfile(profileId: string, selectedModel: string): Promise<void> {
  if (dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired) {
    throw new Error(
      "Codex 配置或恢复记录需要人工处理；应用不会在修复前覆盖文件。",
    );
  }
  if (switchMode === "localProxy") {
    if (!proxyApiAvailable) {
      throw new Error("快速切换暂不可用，请选择“直接配置”后重试。");
    }
    if (localProxy.recoveryRequired) {
      throw new Error("请先关闭快速切换并恢复原设置，再重新开启。");
    }
    const command =
      localProxy.enabled && localProxy.running ? "switch_proxy_route" : "enable_proxy";
    localProxy = await invokeProxyCommand(command, { profileId, selectedModel });
    requireActiveProxySelection(profileId, selectedModel);
    return;
  }

  if (localProxy.enabled || localProxy.recoveryRequired) {
    localProxy = await invokeProxyCommand("disable_proxy");
  }
  await invoke("apply_saved_profile", { profileId, selectedModel });
}

async function refreshProxyStatus(): Promise<void> {
  try {
    localProxy = await invoke<LocalProxyStatus>("proxy_status");
    proxyApiAvailable = true;
  } catch {
    localProxy = stoppedProxy;
    proxyApiAvailable = false;
  }
}

async function invokeProxyCommand(
  command: "enable_proxy" | "switch_proxy_route" | "disable_proxy",
  args?: { profileId: string; selectedModel: string },
): Promise<LocalProxyStatus> {
  try {
    const result = await invoke<LocalProxyStatus>(command, args);
    proxyApiAvailable = true;
    return result;
  } catch (error) {
    const message = String(error);
    if (
      message.includes("not found") ||
      message.includes("unknown command") ||
      message.includes("Command")
    ) {
      proxyApiAvailable = false;
      throw new Error("快速切换暂不可用，请选择“直接配置”后重试。");
    }
    throw new Error(message || "快速切换未能完成。");
  }
}

async function disableLocalProxy(): Promise<void> {
  if (!localProxy.enabled && !localProxy.recoveryRequired) return;
  if (!window.confirm("关闭快速切换，并恢复开启前的 Codex 设置？")) return;
  await run("正在关闭快速切换…", async () => {
    localProxy = await invokeProxyCommand("disable_proxy");
    dashboard = await invoke<DashboardState>("inspect_state");
    await refreshProxyStatus();
    renderDashboard();
    if (
      !proxyApiAvailable ||
      localProxy.enabled ||
      localProxy.running ||
      localProxy.recoveryRequired ||
      (dashboard.current.providerId === "cps-local" &&
        normalizedEndpoint(dashboard.current.baseUrl) ===
          "http://127.0.0.1:15722/v1")
    ) {
      throw new Error(
        "关闭指令已执行，但无法确认本地转发已停止并脱离 Codex；请刷新状态后重试。",
      );
    }
    if (localProxy.lastError) {
      throw new Error(
        "快速切换已停止，但自动启动项未能完全清理；请再次点击关闭或人工检查登录启动项。",
      );
    }
    return localProxy.requiresCodexRestart
      ? "快速切换已关闭。请重新打开 Codex 以完成恢复。"
      : "快速切换已关闭，之前的 Codex 设置已恢复。";
  });
}

async function deleteSavedProfile(profileId: string): Promise<void> {
  const profile = dashboard.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  if (
    (localProxy.enabled || localProxy.recoveryRequired) &&
    localProxy.currentProfileId === profileId
  ) {
    setStatus("这个接入正在使用中。请先切换到其他接入，再将它移除。", "error");
    return;
  }
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
  if (dashboard.recoveryWarnings > 0) {
    setStatus("恢复记录需要人工检查；应用不会在确认前覆盖 Codex 配置。", "error");
    return;
  }
  if (localProxy.enabled || localProxy.recoveryRequired) {
    setStatus("请先关闭快速切换，再恢复之前的 Codex 设置。", "error");
    return;
  }
  if (!dashboard.latestBackup) return;
  if (!window.confirm("撤销上一次设置更改，并恢复之前的 Codex 设置？")) return;
  await run("正在恢复之前的设置…", async () => {
    await invoke("restore_latest");
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    return "之前的设置已恢复。请重新打开 Codex 以生效。";
  });
}

async function openCodex(): Promise<void> {
  if (
    !window.confirm(
      "请先完全退出正在运行的 Codex。确认现在重新打开吗？",
    )
  ) {
    return;
  }
  await run("正在打开 Codex…", async () => {
    await invoke("open_codex");
    await refreshProxyStatus();
    renderDashboard();
    return "Codex 已重新打开，可以继续使用。";
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

function normalizedEndpoint(baseUrl: string | null): string {
  return (baseUrl ?? "").trim().replace(/\/+$/u, "");
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
    required<HTMLButtonElement>("#restore").disabled =
      !dashboard.latestBackup ||
      dashboard.recoveryWarnings > 0 ||
      localProxy.enabled ||
      localProxy.recoveryRequired;
    renderSwitchExperience();
    renderProfiles();
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
  if (message.includes("settings were restored") && message.includes("cleanup is incomplete")) {
    return "快速切换已停止，Codex 原设置也已恢复；本机状态尚未写完，请再次点击“关闭快速切换”完成清理。";
  }
  if (
    message.includes("managed local proxy") ||
    message.includes("fast-switch restore point") ||
    message.includes("activation transaction")
  ) {
    return "快速切换的本机配置或恢复记录已改变。为保护 Codex 设置，请先关闭快速切换完成恢复。";
  }
  if (
    message.includes("local proxy could not start") ||
    message.includes("local proxy credential is missing")
  ) {
    return "本机转发未能启动。请确认端口未被占用，然后重新开启快速切换。";
  }
  if (
    message.includes("non-loopback provider URLs must use HTTPS") ||
    message.includes("provider URL scheme must be HTTPS")
  ) {
    return "远程 Base URL 必须使用 HTTPS；只有这台电脑上的本机地址可以使用 HTTP。";
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
    return "无法安全读取当前 Codex 设置；应用没有进行任何更改。";
  }
  if (message === "[object Object]" || !message.trim()) {
    return "操作失败；Codex 设置没有被修改。";
  }
  return message;
}

function renderConfigReadError(): void {
  required<HTMLElement>("#current-title").textContent = "无法读取当前设置";
  const badge = required<HTMLElement>("#current-badge");
  badge.textContent = "读取失败";
  badge.className = "badge badge-warning";
  required<HTMLElement>("#current-details").innerHTML = `
    <div>
      <span>保护状态</span>
      <strong>没有更改任何设置</strong>
    </div>
    <div>
      <span>下一步</span>
      <strong>确认 Codex 可以正常打开后，再点击“刷新状态”</strong>
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
