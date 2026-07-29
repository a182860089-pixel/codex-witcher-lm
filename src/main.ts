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

interface OfficialProfile {
  schemaVersion: number;
  displayName: string;
  modelId: string | null;
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
  officialProfile: OfficialProfile | null;
  officialProfileWarning: string | null;
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

interface ProfileEditorOptions {
  credentialMissing?: boolean;
}

type SwitchMode = "localProxy" | "directConfig";
type AppPage = "switcher" | "advanced";

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

const previewModel = (
  id: string,
  displayName: string,
  description: string,
): ModelSpec => ({
  id,
  display_name: displayName,
  description,
  context_window: 128_000,
  default_reasoning: "medium",
  reasoning_levels: ["low", "medium", "high"],
  supports_parallel_tool_calls: true,
  supports_images: true,
});

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
  profiles: [
    {
      id: "studio-api",
      display_name: "Studio API",
      base_url: "https://gateway.example.com/v1",
      models: [
        previewModel("code-pro", "Code Pro", "日常开发与复杂任务"),
        previewModel("code-fast", "Code Fast", "快速编码与迭代"),
      ],
      supports_websockets: true,
      credential_required: true,
    },
  ],
  profileWarning: null,
  officialProfile: {
    schemaVersion: 2,
    displayName: "个人 Plus",
    modelId: null,
  },
  officialProfileWarning: null,
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
let activePage: AppPage = "switcher";
let discoveryId: string | null = null;
let discoveredModels: FetchedModel[] = [];
let selectedModels = new Set<string>();
let editingProfileId: string | null = null;
let editingCredentialLoaded = false;
let editingCredentialDirty = false;
let restartNoticeShown = false;
let busy = false;

app.innerHTML = `
  <div class="app-shell">
    <aside class="sidebar" aria-label="应用导航">
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <svg viewBox="0 0 24 24" focusable="false">
            <path d="m7.5 6.5 5.5 5.5-5.5 5.5M13 6.5l5.5 5.5-5.5 5.5" />
          </svg>
        </div>
        <div>
          <strong>Codex Switcher</strong>
          <span>模型与接入</span>
        </div>
      </div>

      <nav class="sidebar-nav">
        <button id="nav-switcher" class="nav-item nav-item-active" type="button" aria-current="page">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 7.5h16M4 16.5h16M8 4v7M16 13v7" />
          </svg>
          <span>模型切换</span>
        </button>
        <button
          id="advanced-settings-toggle"
          class="nav-item"
          type="button"
          aria-label="打开高级设置"
          aria-controls="advanced-settings"
          aria-expanded="false"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-2.82 2.82-.06-.06a1.7 1.7 0 0 0-1.88-.34 1.7 1.7 0 0 0-1.04 1.56V21h-4v-.08A1.7 1.7 0 0 0 8.96 19.36a1.7 1.7 0 0 0-1.88.34l-.06.06-2.82-2.82.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.04H3v-4h.04A1.7 1.7 0 0 0 4.6 8.92a1.7 1.7 0 0 0-.34-1.88L4.2 6.98l2.82-2.82.06.06a1.7 1.7 0 0 0 1.88.34A1.7 1.7 0 0 0 10 3V3h4v.08a1.7 1.7 0 0 0 1.04 1.56 1.7 1.7 0 0 0 1.88-.34l.06-.06 2.82 2.82-.06.06a1.7 1.7 0 0 0-.34 1.88 1.7 1.7 0 0 0 1.56 1.04H21v4h-.04A1.7 1.7 0 0 0 19.4 15Z" />
          </svg>
          <span>高级设置</span>
        </button>
      </nav>

      <div class="sidebar-footer">
        <div id="proxy-health" class="proxy-health" role="status">
          <span class="health-dot" aria-hidden="true"></span>
          <div>
            <span>快速切换</span>
            <strong>正在检查</strong>
          </div>
        </div>
        <span class="version-label">Version 0.3.3</span>
      </div>
    </aside>

    <div class="workspace">
      <header class="app-header">
        <div class="page-heading">
          <p>Codex Provider Switcher</p>
          <h1 id="page-title">模型切换</h1>
          <span id="page-description">选择接入和模型，下一轮对话即可使用。</span>
        </div>
        <button id="refresh" class="button button-toolbar" type="button">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M20 11a8 8 0 1 0-2.34 5.66M20 5v6h-6" />
          </svg>
          <span>刷新</span>
        </button>
      </header>

      <main class="content-scroll">
        <section id="switcher-page" class="page-view" aria-labelledby="current-title">
          <section class="current-section">
            <div class="current-heading">
              <div class="current-symbol" aria-hidden="true">
                <svg viewBox="0 0 24 24">
                  <path d="m8 7 5 5-5 5M13 7l5 5-5 5" />
                </svg>
              </div>
              <div class="current-copy">
                <p id="current-kicker" class="section-kicker">当前模型</p>
                <h2 id="current-title">正在读取…</h2>
              </div>
              <span id="current-badge" class="badge">读取中</span>
            </div>
            <div id="current-details" class="current-details"></div>
          </section>

          <section class="connections-section" aria-labelledby="connections-title">
            <div class="section-title-row">
              <div>
                <h2 id="connections-title">可用接入</h2>
                <p id="connections-help">选择接入和模型，应用会自动完成切换。</p>
              </div>
              <button id="add-connection" class="button button-primary" type="button">
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M12 5v14M5 12h14" />
                </svg>
                <span>添加接入</span>
              </button>
            </div>

            <div class="provider-list">
              <article class="official-section" aria-labelledby="official-title">
                <div class="provider-heading">
                  <div class="provider-icon provider-icon-official" aria-hidden="true">O</div>
                  <div>
                    <p class="section-kicker">Codex 内置</p>
                    <h3 id="official-title">OpenAI 官方账号</h3>
                    <p>登录凭据始终由 Codex 管理。</p>
                  </div>
                  <span id="official-badge" class="badge badge-neutral">未保存</span>
                </div>
                <div id="official-profile" class="official-profile"></div>
              </article>
              <div id="profiles" class="profile-grid"></div>
            </div>
          </section>
        </section>

        <section id="advanced-settings" class="page-view advanced-settings" aria-labelledby="switch-mode-title" hidden>
          <div class="advanced-intro">
            <div>
              <p class="section-kicker">仅在需要时调整</p>
              <h2 id="switch-mode-title">切换与恢复</h2>
              <p>默认推荐快速切换。兼容模式和恢复工具只在排查问题时使用。</p>
            </div>
            <button id="advanced-settings-close" class="button button-secondary" type="button">
              返回模型切换
            </button>
          </div>

          <section class="settings-group" aria-labelledby="mode-heading">
            <div class="settings-group-heading">
              <h3 id="mode-heading">切换方式</h3>
              <p>选择最适合当前 Codex 环境的工作方式。</p>
            </div>
            <div class="mode-options" role="group" aria-label="切换方式">
              <button id="mode-local-proxy" class="mode-option mode-option-active" type="button" aria-pressed="true">
                <span class="mode-radio" aria-hidden="true"></span>
                <span class="mode-option-copy">
                  <span class="mode-option-heading">
                    <strong>快速切换</strong>
                    <span class="mini-badge">推荐</span>
                  </span>
                  <span>应用保持开启时，新一轮对话直接使用新接入和模型。</span>
                </span>
              </button>
              <button id="mode-direct-config" class="mode-option" type="button" aria-pressed="false">
                <span class="mode-radio" aria-hidden="true"></span>
                <span class="mode-option-copy">
                  <span class="mode-option-heading">
                    <strong>直接配置</strong>
                    <span class="mode-label">兼容</span>
                  </span>
                  <span>写入 Codex 用户配置；每次切换后需要重新打开 Codex。</span>
                </span>
              </button>
            </div>
          </section>

          <section class="settings-group" aria-labelledby="service-heading">
            <div class="settings-group-heading">
              <h3 id="service-heading">服务状态</h3>
              <p>这里显示当前模式，并提供安全关闭与恢复操作。</p>
            </div>
            <div class="mode-summary">
              <div>
                <strong id="mode-summary-title">快速切换尚未开启</strong>
                <p id="mode-summary-copy">在模型切换页选择一个接入和模型即可开启。</p>
              </div>
              <div class="mode-summary-actions">
                <button id="restore" class="button button-quiet" type="button">撤销上次更改</button>
                <button id="open-codex" class="button button-secondary" type="button" hidden>重新打开 Codex</button>
                <button id="stop-proxy" class="button button-secondary danger-text" type="button" hidden>关闭快速切换</button>
              </div>
            </div>
          </section>

          <aside class="privacy-note">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 3 5 6v5c0 4.7 2.9 8 7 10 4.1-2 7-5.3 7-10V6l-7-3Z" />
              <path d="m9 12 2 2 4-4" />
            </svg>
            <div>
              <strong>本机优先</strong>
              <p>API Key 只保存在系统密钥库中；官方登录和 OAuth 令牌始终由 Codex 管理。</p>
            </div>
          </aside>
        </section>
      </main>

      <output id="status" class="status status-info" aria-live="polite">正在读取当前状态…</output>
    </div>
  </div>

  <dialog id="editor" class="editor-dialog" aria-labelledby="editor-title">
    <div class="editor-shell">
      <header class="editor-header">
        <div>
          <p id="editor-kicker" class="section-kicker">添加接入</p>
          <h2 id="editor-title">连接你的模型服务</h2>
          <p>通常只需要 Base URL 和 API Key。</p>
        </div>
        <button id="close-editor" class="button button-icon" type="button" aria-label="关闭">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m7 7 10 10M17 7 7 17" />
          </svg>
        </button>
      </header>

      <div class="editor-progress" aria-label="设置进度">
        <div id="editor-step-connection" class="progress-step progress-step-active">
          <span>1</span>
          <strong>连接信息</strong>
        </div>
        <div class="progress-line" aria-hidden="true"></div>
        <div id="editor-step-models" class="progress-step">
          <span>2</span>
          <strong>选择模型</strong>
        </div>
      </div>

      <div class="editor-body">
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
            <small id="api-key-help" class="field-help">Key 只会保存在这台电脑的系统密钥库中。</small>
          </label>
        </div>

        <div class="editor-actions first-step-actions">
          <button id="fetch-models" class="button button-primary" type="button">
            连接并获取模型
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
          </button>
        </div>

        <div id="model-step" class="model-step" hidden>
          <div class="model-toolbar">
            <div>
              <h3>选择要保留的模型</h3>
              <p>以后快速切换时，只显示这些模型。</p>
            </div>
            <strong id="selected-count">已选择 0 个</strong>
          </div>
          <div class="model-controls">
            <div class="search-field">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <circle cx="11" cy="11" r="6" />
                <path d="m16 16 4 4" />
              </svg>
              <input id="model-search" type="search" autocomplete="off" placeholder="搜索模型" />
            </div>
            <button id="select-visible" class="button button-secondary" type="button">全选结果</button>
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
      </div>
    </div>
  </dialog>

  <dialog id="restart-notice" class="restart-dialog" aria-labelledby="restart-notice-title">
    <div class="restart-dialog-mark" aria-hidden="true">✓</div>
    <h2 id="restart-notice-title">快速切换已准备好</h2>
    <p>重新打开一次 Codex 即可生效。之后保持本软件运行，模型切换会应用到下一轮对话。</p>
    <div class="restart-dialog-actions">
      <button id="restart-later" class="button button-secondary" type="button">稍后</button>
      <button id="restart-now" class="button button-primary" type="button">重新打开 Codex</button>
    </div>
  </dialog>
`;

const status = required<HTMLOutputElement>("#status");

required<HTMLButtonElement>("#refresh").addEventListener("click", refreshDashboard);
required<HTMLButtonElement>("#nav-switcher").addEventListener("click", () =>
  setAdvancedSettingsVisible(false),
);
required<HTMLButtonElement>("#advanced-settings-toggle").addEventListener("click", () =>
  setAdvancedSettingsVisible(true),
);
required<HTMLButtonElement>("#advanced-settings-close").addEventListener("click", () => {
  setAdvancedSettingsVisible(false);
  required<HTMLButtonElement>("#nav-switcher").focus();
});
required<HTMLButtonElement>("#mode-local-proxy").addEventListener("click", () =>
  selectSwitchMode("localProxy"),
);
required<HTMLButtonElement>("#mode-direct-config").addEventListener("click", () =>
  selectSwitchMode("directConfig"),
);
required<HTMLButtonElement>("#stop-proxy").addEventListener("click", disableLocalProxy);
required<HTMLButtonElement>("#restore").addEventListener("click", restoreLatest);
required<HTMLButtonElement>("#open-codex").addEventListener("click", () => openCodex());
required<HTMLButtonElement>("#add-connection").addEventListener("click", openEditor);
required<HTMLElement>("#official-profile").addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof HTMLButtonElement)) return;
  if (target.dataset.officialAction === "activate") void activateOfficial();
  if (target.dataset.officialAction === "save") void saveCurrentOfficial();
  if (target.dataset.officialAction === "restart") void openCodex();
});
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
required<HTMLInputElement>("#api-key").addEventListener("input", () => {
  if (editingProfileId && editingCredentialLoaded) {
    editingCredentialDirty = true;
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
required<HTMLButtonElement>("#restart-later").addEventListener("click", () =>
  required<HTMLDialogElement>("#restart-notice").close(),
);
required<HTMLButtonElement>("#restart-now").addEventListener("click", () => {
  required<HTMLDialogElement>("#restart-notice").close();
  void openCodex(false);
});
required<HTMLDialogElement>("#editor").addEventListener("cancel", (event) => {
  event.preventDefault();
  void closeEditor();
});

void refreshDashboard().then(showRequestedBrowserPreview);

function showRequestedBrowserPreview(): void {
  if (nativeAvailable) return;
  const preview = new URLSearchParams(window.location.search).get("preview");
  if (preview === "advanced") {
    setAdvancedSettingsVisible(true);
    return;
  }
  if (preview === "editor") {
    openEditor();
    input("#connection-name").value = "Studio API";
    input("#base-url").value = "https://gateway.example.com/v1";
    input("#api-key").value = "demo-key";
  }
}

function setAdvancedSettingsVisible(visible: boolean): void {
  const panel = required<HTMLElement>("#advanced-settings");
  const switcher = required<HTMLElement>("#switcher-page");
  const toggle = required<HTMLButtonElement>("#advanced-settings-toggle");
  const switcherNav = required<HTMLButtonElement>("#nav-switcher");
  const pageTitle = required<HTMLElement>("#page-title");
  const pageDescription = required<HTMLElement>("#page-description");
  activePage = visible ? "advanced" : "switcher";
  const advancedActive = activePage === "advanced";
  panel.hidden = !advancedActive;
  switcher.hidden = advancedActive;
  toggle.classList.toggle("nav-item-active", advancedActive);
  switcherNav.classList.toggle("nav-item-active", !advancedActive);
  toggle.setAttribute("aria-expanded", String(advancedActive));
  toggle.setAttribute("aria-current", advancedActive ? "page" : "false");
  switcherNav.setAttribute("aria-current", advancedActive ? "false" : "page");
  pageTitle.textContent = advancedActive ? "高级设置" : "模型切换";
  pageDescription.textContent = advancedActive
    ? "切换工作方式，查看服务状态或安全恢复设置。"
    : "选择接入和模型，下一轮对话即可使用。";
  required<HTMLElement>(".content-scroll").scrollTo({
    top: 0,
    behavior: "smooth",
  });
}

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
    if (dashboard.officialProfileWarning) {
      return "保存的官方配置暂时无法读取；原有内容没有被覆盖。";
    }
    if (!proxyApiAvailable) {
      return "已读取当前设置。快速切换暂不可用，可从高级设置使用兼容配置。";
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
  const officialIsActive = isOfficialActive(proxyIsActive);
  const currentConnection = proxyIsActive
    ? (proxyProfile?.display_name ?? "已保存的接入")
    : officialIsActive
      ? (dashboard.officialProfile?.displayName ?? "OpenAI 官方账号")
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
  currentKicker.textContent = "当前模型";
  if (dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired) {
    currentBadge.textContent = "需人工处理";
    currentBadge.className = "badge badge-warning";
  } else if (localProxy.recoveryRequired) {
    currentBadge.textContent = "可安全恢复";
    currentBadge.className = "badge badge-warning";
  } else if (proxyIsActive) {
    currentBadge.textContent = "快速切换";
    currentBadge.className = "badge";
  } else if (officialIsActive) {
    currentBadge.textContent = "官方账号";
    currentBadge.className = "badge badge-official";
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
      <span>服务地址</span>
      <strong>${escapeHtml(readableEndpoint(currentEndpoint))}</strong>
    </div>
    <div>
      <span>凭据方式</span>
      <strong>${escapeHtml(currentAuth)}</strong>
    </div>
  `;
  renderSwitchExperience();
  renderOfficialProfile();
  renderProfiles();
  required<HTMLButtonElement>("#restore").disabled =
    !dashboard.latestBackup ||
    dashboard.recoveryWarnings > 0 ||
    localProxy.enabled ||
    localProxy.recoveryRequired ||
    busy;
  maybeShowRestartNotice();
}

function renderOfficialProfile(): void {
  const root = required<HTMLElement>("#official-profile");
  const badge = required<HTMLElement>("#official-badge");
  const proxyIsActive =
    localProxy.enabled && localProxy.running && !localProxy.recoveryRequired;
  const current = isOfficialActive(proxyIsActive);
  const saved = dashboard.officialProfile;
  const blocked =
    busy ||
    dashboard.recoveryWarnings > 0 ||
    localProxy.manualRecoveryRequired ||
    localProxy.recoveryRequired;

  if (dashboard.officialProfileWarning) {
    badge.textContent = "需检查";
    badge.className = "badge badge-warning";
  } else if (current) {
    badge.textContent = "当前使用";
    badge.className = "badge badge-official";
  } else if (saved) {
    badge.textContent = "已保存";
    badge.className = "badge badge-neutral";
  } else {
    badge.textContent = "未保存";
    badge.className = "badge badge-neutral";
  }

  const savedModel = saved?.modelId ?? "由 Codex 自动选择";
  const savedName = saved?.displayName ?? "OpenAI 官方账号";
  const mainAction = saved ? `切换到 ${savedName}` : "切换到官方登录";
  const explanation = current
    ? "当前已使用 Codex 内置 OpenAI 接入。完成登录并选好模型后，可保存这份无凭据配置。"
    : saved
      ? `已保存账号配置：${savedName}；模型：${savedModel}。切换后需要重新打开 Codex。`
      : "先切换到 Codex 内置 OpenAI 接入，重新打开 Codex 并按提示登录；随后返回保存当前配置。";
  const nameEditor = current
    ? `
      <label class="official-name-field">
        <span>配置名称</span>
        <input
          id="official-profile-name"
          maxlength="80"
          autocomplete="off"
          value="${escapeHtml(savedName)}"
          placeholder="例如：个人 Plus 或工作账号"
        />
        <small class="field-help">用于区分这份官方登录配置。本软件不会读取邮箱、用户名或 OAuth 令牌。</small>
      </label>
    `
    : "";

  root.innerHTML = `
    <div class="official-main">
      <div class="official-copy">
        <p>${escapeHtml(explanation)}</p>
        <span>官方账号与 API 接入互换后需要重启 Codex；API 接入之间仍可快速切换。</span>
      </div>
      ${nameEditor}
    </div>
    <div class="official-actions">
      <button class="button button-primary" data-official-action="activate" type="button" ${current ? "hidden" : ""} ${blocked || dashboard.officialProfileWarning ? "disabled" : ""}>${mainAction}</button>
      <button class="button button-secondary" data-official-action="save" type="button" ${!current || blocked ? "disabled" : ""}>${saved ? "更新官方配置" : "保存官方配置"}</button>
      <button class="button button-quiet" data-official-action="restart" type="button" ${!current || blocked ? "disabled" : ""}>重新打开 Codex</button>
    </div>
  `;
}

function isOfficialActive(proxyIsActive = false): boolean {
  return (
    !proxyIsActive &&
    dashboard.current.providerId === "openai" &&
    dashboard.current.baseUrl === null &&
    dashboard.current.authKind === "officialLogin"
  );
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
    openCodexButton.hidden = true;
  } else if (switchMode === "directConfig") {
    summaryTitle.textContent = "直接配置（兼容）";
    summaryCopy.textContent = localProxy.enabled || localProxy.recoveryRequired
      ? "切换时会先安全关闭快速切换，再更新 Codex 设置。"
      : "适合快速切换不可用的情况；切换后需要重新打开 Codex。";
    connectionsHelp.textContent = "选择模型后会更新 Codex 设置，完成后需要重新打开 Codex。";
    saveAndSwitch.textContent = "保存并写入配置";
    openCodexButton.hidden = false;
  } else if (!proxyApiAvailable) {
    summaryTitle.textContent = "快速切换暂不可用";
    summaryCopy.textContent = "你仍可选择“直接配置”完成模型切换。";
    connectionsHelp.textContent = "快速切换暂不可用，请从右上角高级设置选择直接配置。";
    saveAndSwitch.textContent = "保存并使用";
    openCodexButton.hidden = true;
  } else if (localProxy.recoveryRequired) {
    summaryTitle.textContent = "快速切换需要修复";
    summaryCopy.textContent = "请先关闭快速切换并恢复原设置，再重新选择接入和模型。";
    connectionsHelp.textContent = "修复完成前不会发送新的本地转发请求。";
    saveAndSwitch.textContent = "请先完成修复";
    openCodexButton.hidden = true;
  } else if (localProxy.running) {
    summaryTitle.textContent = "快速切换已开启";
    summaryCopy.textContent = "应用保持开启时，可直接切换接入和模型。";
    connectionsHelp.textContent = "选择模型即可使用，也可以编辑已有接入。";
    saveAndSwitch.textContent = "保存并使用";
    openCodexButton.hidden = true;
  } else if (localProxy.enabled) {
    summaryTitle.textContent = "快速切换需要重新开启";
    summaryCopy.textContent = "选择一个接入和模型即可安全修复；也可以关闭并恢复原设置。";
    connectionsHelp.textContent = "重新选择接入和模型后，应用会检查并修复快速切换。";
    saveAndSwitch.textContent = "保存并重新开启";
    openCodexButton.hidden = true;
  } else {
    summaryTitle.textContent = "快速切换尚未开启";
    summaryCopy.textContent = "选择一个接入和模型即可开启。";
    connectionsHelp.textContent = "选择接入和模型，应用会自动配置代理服务。";
    saveAndSwitch.textContent = "保存并使用";
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
        <div class="empty-state-icon" aria-hidden="true">+</div>
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
            : "使用此模型";
      const initial =
        Array.from(profile.display_name.trim())[0]?.toLocaleUpperCase() ?? "A";
      return `
        <article class="profile-card ${isCurrent ? "profile-card-current" : ""}">
          <div class="profile-heading">
            <div class="provider-icon provider-icon-api" aria-hidden="true">${escapeHtml(initial)}</div>
            <div>
              <h3>${escapeHtml(profile.display_name)}</h3>
              <p>${escapeHtml(readableEndpoint(profile.base_url))}</p>
            </div>
            ${isCurrent ? '<span class="badge">当前</span>' : ""}
          </div>
          <label class="profile-model">
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
          <div class="profile-actions">
            <button class="button button-primary" data-switch-profile="${escapeHtml(profile.id)}" type="button" ${manualRecoveryBlocked || (switchMode === "localProxy" && localProxy.recoveryRequired) ? "disabled" : ""}>${actionLabel}</button>
            <button class="button button-secondary" data-edit-profile="${escapeHtml(profile.id)}" type="button">编辑配置</button>
            <button class="button button-quiet danger-text" data-delete-profile="${escapeHtml(profile.id)}" type="button">移除</button>
          </div>
        </article>
      `;
    })
    .join("");

  root.querySelectorAll<HTMLButtonElement>("[data-switch-profile]").forEach((button) => {
    button.addEventListener("click", () => switchSavedProfile(button.dataset.switchProfile ?? ""));
  });
  root.querySelectorAll<HTMLButtonElement>("[data-edit-profile]").forEach((button) => {
    button.addEventListener("click", () => {
      void openProfileEditor(button.dataset.editProfile ?? "");
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-delete-profile]").forEach((button) => {
    button.addEventListener("click", () => deleteSavedProfile(button.dataset.deleteProfile ?? ""));
  });
}

async function activateOfficial(): Promise<void> {
  if (dashboard.recoveryWarnings > 0 || localProxy.manualRecoveryRequired) {
    setStatus("配置或恢复记录需要人工处理；应用不会在修复前写入 Codex 配置。", "error");
    return;
  }
  await run("正在切换到 OpenAI 官方账号…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用切换官方账号配置。");
    if (localProxy.enabled || localProxy.recoveryRequired) {
      localProxy = await invokeProxyCommand("disable_proxy");
      if (localProxy.enabled || localProxy.running || localProxy.recoveryRequired) {
        throw new Error("快速切换尚未安全关闭，官方配置没有继续写入。");
      }
    }
    await invoke(
      dashboard.officialProfile ? "activate_official_profile" : "prepare_official_login",
    );
    dashboard = await invoke<DashboardState>("inspect_state");
    await refreshProxyStatus();
    renderDashboard();
    if (!isOfficialActive(false)) {
      throw new Error("官方配置已写入，但 Codex 路由被其他程序立即改变。");
    }
    return dashboard.officialProfile
      ? `已切换到 ${dashboard.officialProfile.displayName}。请完全退出并重新打开 Codex。`
      : "已准备 OpenAI 官方登录。请重新打开 Codex，按提示登录；登录后返回保存当前官方配置。";
  });
}

async function saveCurrentOfficial(): Promise<void> {
  const displayName = required<HTMLInputElement>("#official-profile-name").value.trim();
  if (!displayName) {
    setStatus("请先填写官方配置名称。", "error");
    required<HTMLInputElement>("#official-profile-name").focus();
    return;
  }
  await run("正在保存当前官方配置…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用保存官方配置。");
    const saved = await invoke<OfficialProfile>("save_current_official_profile", {
      displayName,
    });
    dashboard = await invoke<DashboardState>("inspect_state");
    renderDashboard();
    return saved.modelId
      ? `已保存 ${saved.displayName}，当前模型为 ${saved.modelId}。账号令牌仍由 Codex 管理。`
      : `已保存 ${saved.displayName}，模型由 Codex 自动选择。账号令牌仍由 Codex 管理。`;
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
  if (discoveryId && nativeAvailable) {
    const sessionId = discoveryId;
    void invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  discoveryId = null;
  discoveredModels = [];
  selectedModels.clear();
  editingProfileId = null;
  editingCredentialLoaded = false;
  editingCredentialDirty = false;
  input("#connection-name").value = "";
  input("#base-url").value = "";
  input("#api-key").value = "";
  input("#model-search").value = "";
  input("#manual-model").value = "";
  required<HTMLElement>("#model-step").hidden = true;
  required<HTMLElement>("#editor-kicker").textContent = "添加接入";
  required<HTMLElement>("#editor-title").textContent = "连接你的模型服务";
  required<HTMLInputElement>("#api-key").placeholder = "输入 API Key";
  required<HTMLElement>("#api-key-help").textContent =
    "Key 只会保存在这台电脑的系统密钥库中。";
  required<HTMLButtonElement>("#fetch-models").textContent = "连接并获取模型";
  required<HTMLButtonElement>("#save-only").textContent = "仅保存";
  required<HTMLButtonElement>("#save-and-switch").textContent = "保存并使用";
  setEditorStep(1);
  const editor = required<HTMLDialogElement>("#editor");
  if (!editor.open) editor.showModal();
  required<HTMLInputElement>("#connection-name").focus();
}

async function openProfileEditor(
  profileId: string,
  options: ProfileEditorOptions = {},
): Promise<void> {
  const profile = dashboard.profiles.find((item) => item.id === profileId);
  if (!profile) return;
  if (discoveryId && nativeAvailable) {
    const sessionId = discoveryId;
    void invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  editingProfileId = profile.id;
  editingCredentialLoaded = false;
  editingCredentialDirty = false;
  discoveryId = null;
  discoveredModels = profile.models.map((model) => ({ id: model.id, ownedBy: null }));
  selectedModels = new Set(profile.models.map((model) => model.id));
  required<HTMLElement>("#editor-kicker").textContent = "编辑配置";
  required<HTMLElement>("#editor-title").textContent = profile.display_name;
  input("#connection-name").value = profile.display_name;
  input("#base-url").value = profile.base_url;
  input("#api-key").value = "";
  input("#api-key").type = "password";
  input("#api-key").placeholder = "正在读取现有 Key…";
  required<HTMLButtonElement>("#toggle-key").textContent = "显示";
  required<HTMLElement>("#api-key-help").textContent =
    "正在从这台电脑的系统密钥库读取现有 Key。";
  required<HTMLButtonElement>("#fetch-models").textContent = "重新获取模型";
  required<HTMLButtonElement>("#save-only").textContent = "保存配置";
  required<HTMLButtonElement>("#save-and-switch").textContent = "保存并使用";
  input("#model-search").value = "";
  input("#manual-model").value = "";
  required<HTMLElement>("#model-step").hidden = false;
  setEditorStep(2);
  const editor = required<HTMLDialogElement>("#editor");
  if (!editor.open) editor.showModal();
  renderModelChoices();
  const activeModel =
    localProxy.enabled && localProxy.currentProfileId === profile.id
      ? localProxy.currentModelId
      : dashboard.current.providerId === profile.id
        ? dashboard.current.modelId
        : null;
  if (activeModel && selectedModels.has(activeModel)) {
    required<HTMLSelectElement>("#default-model").value = activeModel;
  }
  if (options.credentialMissing) {
    input("#api-key").placeholder = "重新输入 API Key";
    required<HTMLElement>("#api-key-help").textContent =
      "这个接入的 Key 已不在系统密钥库中。请重新输入一次，再获取模型或保存配置。";
    input("#api-key").focus();
    return;
  }
  input("#connection-name").focus();
  await run(
    "正在读取现有 Key…",
    async () => {
      if (!nativeAvailable) throw new Error("请通过桌面应用读取已保存的 Key。");
      const secret = await invoke<string>("load_profile_credential", { profileId });
      if (editingProfileId !== profileId) return "编辑窗口已切换。";
      const keyField = input("#api-key");
      keyField.value = secret;
      keyField.type = "text";
      keyField.placeholder = "输入 API Key";
      required<HTMLButtonElement>("#toggle-key").textContent = "隐藏";
      required<HTMLElement>("#api-key-help").textContent =
        "已从系统密钥库载入。可直接修改；不改动时会继续使用原 Key。";
      editingCredentialLoaded = true;
      editingCredentialDirty = false;
      return "现有 Key 已载入，可以直接编辑配置。";
    },
    (error) => {
      if (editingProfileId !== profileId) return;
      const missing = isMissingProviderCredential(error);
      input("#api-key").placeholder = missing ? "重新输入 API Key" : "无法读取现有 Key";
      required<HTMLElement>("#api-key-help").textContent = missing
        ? "这个接入的 Key 已不在系统密钥库中。请重新输入一次，再获取模型或保存配置。"
        : "系统密钥库暂时无法读取。为避免误用旧配置，请重新输入 API Key 后再保存。";
      if (missing) window.setTimeout(() => input("#api-key").focus(), 0);
    },
  );
}

async function closeEditor(): Promise<void> {
  if (discoveryId && nativeAvailable) {
    const sessionId = discoveryId;
    discoveryId = null;
    await invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  discoveredModels = [];
  selectedModels.clear();
  editingProfileId = null;
  editingCredentialLoaded = false;
  editingCredentialDirty = false;
  const editor = required<HTMLDialogElement>("#editor");
  if (editor.open) editor.close();
  required<HTMLElement>("#model-step").hidden = true;
  setEditorStep(1);
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
      setEditorStep(2);
      renderModelChoices();
      throw error;
    }
    if (!editingProfileId) {
      keyInput.value = "";
    }
    discoveryId = result.sessionId;
    input("#base-url").value = result.baseUrl;
    discoveredModels = result.models;
    selectedModels.clear();
    required<HTMLElement>("#model-step").hidden = false;
    setEditorStep(2);
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
  const originalProfile = editingProfileId
    ? dashboard.profiles.find((profile) => profile.id === editingProfileId)
    : null;

  const mode = switchMode;
  await run(activate ? "正在保存并使用…" : "正在保存配置…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用保存接入。");
    let sessionForSave = discoveryId;
    let resolvedBaseUrl = baseUrl;
    if (!sessionForSave) {
      const keyField = input("#api-key");
      const secret = keyField.value.trim();
      const canReuseExistingCredential =
        originalProfile &&
        normalizedEndpoint(originalProfile.base_url) === normalizedEndpoint(baseUrl) &&
        editingCredentialLoaded &&
        !editingCredentialDirty;
      if (canReuseExistingCredential) {
        resolvedBaseUrl = originalProfile.base_url;
      } else if (secret) {
        const staged = await invoke<CredentialSessionSummary>("stage_credential", {
          input: { baseUrl, secret },
        });
        keyField.value = "";
        sessionForSave = staged.sessionId;
        resolvedBaseUrl = staged.baseUrl;
      } else if (!originalProfile) {
        throw new Error("请输入 API Key。");
      }
    }
    const profile = buildProfile(displayName, resolvedBaseUrl, modelIds);
    const connectionChanged = activate && isCrossConnectionSwitch(profile.id);
    const activeProxyModel =
      localProxy.enabled && localProxy.currentProfileId === profile.id
        ? localProxy.currentModelId
        : null;
    if (activeProxyModel && !modelIds.includes(activeProxyModel) && !activate) {
      throw new Error("当前模型正在使用中；如需移除，请选择新的模型并点击“保存并使用”。");
    }
    const profileForFirstSave =
      activeProxyModel && !modelIds.includes(activeProxyModel)
        ? buildProfile(displayName, resolvedBaseUrl, [...modelIds, activeProxyModel])
        : profile;
    discoveryId = null;
    try {
      await invoke("save_profile", {
        input: { profile: profileForFirstSave, discoveryId: sessionForSave },
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
        if (profileForFirstSave.models.length !== profile.models.length) {
          await invoke("save_profile", {
            input: { profile, discoveryId: null },
          });
        }
      } catch (error) {
        await refreshDashboard();
        const action = mode === "directConfig" ? "配置切换" : "快速切换";
        throw new Error(`接入已保存；${action}未能确认：${friendlyError(error)}`);
      }
    } else if (
      activeProxyModel &&
      localProxy.running &&
      !localProxy.recoveryRequired
    ) {
      localProxy = await invokeProxyCommand("switch_proxy_route", {
        profileId: profile.id,
        selectedModel: activeProxyModel,
      });
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
      return `已保存 ${displayName} 的配置。`;
    }
    if (mode === "directConfig") {
      return `已写入 ${displayName} / ${defaultModel}。请重新打开 Codex 以生效。${crossConnectionAdvice(connectionChanged)}`;
    }
    return `当前模型：${displayName} / ${defaultModel}。${crossConnectionAdvice(connectionChanged)}`;
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
  let credentialMissing = false;
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
      return `当前模型：${profile.display_name} / ${selectedModel}。${crossConnectionAdvice(connectionChanged)}`;
    },
    (error) => {
      credentialMissing = isMissingProviderCredential(error);
    },
  );
  if (credentialMissing) {
    await openProfileEditor(profileId, { credentialMissing: true });
  }
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
      throw new Error("快速切换暂不可用，请从高级设置选择“直接配置”后重试。");
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
      throw new Error("快速切换暂不可用，请从高级设置选择“直接配置”后重试。");
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

async function openCodex(confirmFirst = true): Promise<void> {
  if (
    confirmFirst &&
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

function maybeShowRestartNotice(): void {
  if (
    restartNoticeShown ||
    !nativeAvailable ||
    !localProxy.running ||
    !localProxy.requiresCodexRestart
  ) {
    return;
  }
  const dialog = required<HTMLDialogElement>("#restart-notice");
  restartNoticeShown = true;
  if (!dialog.open) dialog.showModal();
}

function buildProfile(
  displayName: string,
  baseUrl: string,
  modelIds: string[],
): ProviderProfile {
  return {
    id: editingProfileId ?? providerId(displayName, baseUrl),
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
      officialLogin: "Codex 官方登录",
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
    renderOfficialProfile();
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
  if (isMissingProviderCredential(error)) {
    return "这个接入的 API Key 已不在系统密钥库中，请编辑配置并重新输入一次。";
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
    message.includes("built-in OpenAI login") ||
    message.includes("official configuration before activating")
  ) {
    return "请先切换到 Codex 官方登录并重新打开 Codex；完成登录后再保存当前官方配置。";
  }
  if (message.includes("official configuration")) {
    return "官方配置无法安全读取或保存；原有 Codex 登录凭据没有被修改。";
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

function isMissingProviderCredential(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return (
    message.includes("saved provider credential is missing") ||
    message.includes("credential is not stored")
  );
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
  setEditorStep(1);
  if (nativeAvailable) {
    void invoke("cancel_discovery", { sessionId }).catch(() => undefined);
  }
  setStatus("Base URL 已改变，请重新输入 API Key 并获取模型。", "info");
}

function setEditorStep(step: 1 | 2): void {
  required<HTMLElement>("#editor-step-connection").classList.toggle(
    "progress-step-active",
    step === 1,
  );
  required<HTMLElement>("#editor-step-models").classList.toggle(
    "progress-step-active",
    step === 2,
  );
  required<HTMLElement>(".progress-line").classList.toggle(
    "progress-line-complete",
    step === 2,
  );
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
