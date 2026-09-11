import { invoke } from "@tauri-apps/api/core";
import { animateDialogIn, animateDialogOut, animatePage } from "./motion";
import "./styles.css";
import "./styles-polish.css";

type ReasoningEffort = "low" | "medium" | "high";
type StatusKind = "info" | "working" | "success" | "error";
type CodexAuthMode = "none" | "apiKey" | "chatgpt" | "other";
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
  email?: string | null;
  planType?: string | null;
}

interface CodexAccountStatus {
  authMode: CodexAuthMode;
  email: string | null;
  planType: string | null;
  requiresOpenaiAuth: boolean;
  codexAccessTokenEnvironmentPresent: boolean;
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

type OutboundProxyMode = "auto" | "direct" | "system";

interface LocalProxyStatus {
  enabled: boolean;
  running: boolean;
  recoveryRequired: boolean;
  manualRecoveryRequired: boolean;
  currentProfileId: string | null;
  currentModelId: string | null;
  requiresCodexRestart: boolean;
  lastError: string | null;
  ccSwitchDetected: boolean;
  outboundProxyMode: OutboundProxyMode;
}

interface RepairFastSwitchReport {
  steps: string[];
  status: LocalProxyStatus;
}

interface OutboundProxyStatus {
  detected: boolean;
  proxyUrl: string | null;
  source: string;
  host: string | null;
  port: number | null;
  listening: boolean | null;
  listenerHint: string | null;
  candidates: Array<{
    proxyUrl: string;
    host: string;
    port: number;
    listening: boolean;
    source: string;
  }>;
  summary: string;
  detail: string;
}

interface OutboundProbeResult {
  mode: string;
  url: string;
  ok: boolean;
  status: number | null;
  latencyMs: number;
  message: string;
}

interface OutboundNetworkReport {
  proxy: OutboundProxyStatus;
  probeBase: string;
  direct: OutboundProbeResult;
  viaProxy: OutboundProbeResult | null;
  verdict: string;
  recommendations: string[];
  startedProxy: string | null;
}

interface AppUpdateStatus {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  skipped: boolean;
  releaseNotes: string;
  releaseUrl: string;
  downloadUrl: string;
  assetName: string | null;
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
    schemaVersion: 3,
    displayName: "alex@example.com",
    modelId: null,
    email: "alex@example.com",
    planType: "plus",
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
  ccSwitchDetected: false,
  outboundProxyMode: "auto",
};

const browserAccountPreview: CodexAccountStatus = {
  authMode: "chatgpt",
  email: "alex@example.com",
  planType: "plus",
  requiresOpenaiAuth: true,
  codexAccessTokenEnvironmentPresent: false,
};

const unavailableAccountStatus: CodexAccountStatus = {
  authMode: "none",
  email: null,
  planType: null,
  requiresOpenaiAuth: false,
  codexAccessTokenEnvironmentPresent: false,
};

let dashboard = browserPreview;
let nativeAvailable = "__TAURI_INTERNALS__" in window;
let proxyApiAvailable = true;
let localProxy = stoppedProxy;
let outboundProxy: OutboundProxyStatus | null = null;
let outboundReport: OutboundNetworkReport | null = null;
let codexAccount = browserAccountPreview;
let codexAccountAvailable = !nativeAvailable;
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
let pendingUpdate: AppUpdateStatus | null = null;

app.innerHTML = `
  <div class="app-shell">
    <aside class="sidebar" aria-label="应用导航">
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <img src="/icon.png" alt="" />
        </div>
        <div class="brand-copy">
          <strong>LM Codex Switch</strong>
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
        <div id="network-health" class="proxy-health" role="status">
          <span class="health-dot" aria-hidden="true"></span>
          <div>
            <span>出站代理</span>
            <strong>自动检测中</strong>
          </div>
        </div>
        <span id="app-version" class="version-label">Version 0.3.5</span>
      </div>
    </aside>

    <div class="workspace">
      <header class="app-header">
        <div class="page-heading">
          <p>LM Codex Switch</p>
          <h1 id="page-title">模型切换</h1>
          <span id="page-description" hidden></span>
        </div>
        <div class="header-actions">
          <button id="restart-codex" class="button button-toolbar" type="button" title="强制结束 Codex 相关进程并重新打开">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 6v6l4 2" />
              <circle cx="12" cy="12" r="8" />
              <path d="M16.5 7.5 19 5M19 5v4h-4" />
            </svg>
            <span>重启 Codex</span>
          </button>
          <button id="check-update" class="button button-toolbar" type="button" title="检查 GitHub 发布页是否有新版本">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 5v10M8 11l4 4 4-4" />
              <path d="M6 19h12" />
            </svg>
            <span>检测更新</span>
          </button>
          <button id="refresh" class="button button-toolbar" type="button">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M20 11a8 8 0 1 0-2.34 5.66M20 5v6h-6" />
            </svg>
            <span>刷新</span>
          </button>
        </div>
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
            <p id="current-https-ip-warning" class="current-https-ip-warning" hidden></p>
            <div id="current-proxy-actions" class="current-proxy-actions" hidden>
              <button id="repair-proxy" class="button button-primary" type="button">一键修复</button>
              <button id="stop-proxy-main" class="button button-secondary danger-text" type="button">关闭快速切换</button>
            </div>
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

          <section class="settings-group" aria-labelledby="outbound-heading">
            <div class="settings-group-heading">
              <h3 id="outbound-heading">出站代理</h3>
              <p>默认自动：检测到系统/环境代理在监听则走代理（跳过回环），否则直连。也可手动选择始终直连或始终走系统代理。</p>
            </div>
            <div class="mode-summary outbound-summary">
              <div>
                <strong id="outbound-summary-title">尚未检测</strong>
                <p id="outbound-summary-copy">打开高级设置或点击检测后，会显示当前出站代理。</p>
                <div id="outbound-report" class="outbound-report" hidden></div>
              </div>
              <div class="mode-summary-actions">
                <label class="outbound-mode-field">
                  <span>出站方式</span>
                  <select id="outbound-proxy-mode">
                    <option value="auto">自动（有系统代理则走代理）</option>
                    <option value="direct">始终直连</option>
                    <option value="system">始终走系统代理</option>
                  </select>
                </label>
                <button id="detect-outbound" class="button button-secondary" type="button">重新检测</button>
                <button id="diagnose-outbound" class="button button-primary" type="button">诊断上游网络</button>
                <button id="ensure-outbound" class="button button-quiet" type="button">检测并尝试启动</button>
              </div>
            </div>
          </section>

          <section class="settings-group" aria-labelledby="service-heading">
            <div class="settings-group-heading">
              <h3 id="service-heading">服务状态</h3>
              <p>这里显示当前模式，并提供安全关闭与恢复操作。</p>
            </div>
            <div class="mode-summary">
              <div>
                <strong id="mode-summary-title">快速切换将在首次使用时开启</strong>
                <p id="mode-summary-copy">首次保存并使用 API 接入时，应用会自动启用本机代理。</p>
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
          <h2 id="editor-title">选择接入方式</h2>
          <p id="editor-description">使用 OpenAI 官方账号，或连接兼容的 API 服务。</p>
        </div>
        <button id="close-editor" class="button button-icon" type="button" aria-label="关闭">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m7 7 10 10M17 7 7 17" />
          </svg>
        </button>
      </header>

      <div id="connection-type-step" class="connection-type-step">
        <div class="connection-type-options" role="group" aria-label="接入方式">
          <button id="choose-official-connection" class="connection-type-option" type="button">
            <span class="connection-type-icon connection-type-icon-official" aria-hidden="true">O</span>
            <span>
              <strong>OpenAI 官方登录</strong>
              <small>在浏览器中登录，由 Codex 管理账号</small>
            </span>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
          </button>
          <button id="choose-api-connection" class="connection-type-option" type="button">
            <span class="connection-type-icon connection-type-icon-api" aria-hidden="true">
              <svg viewBox="0 0 24 24"><circle cx="8" cy="12" r="3" /><path d="M11 12h9M17 9v6" /></svg>
            </span>
            <span>
              <strong>API 接入</strong>
              <small>填写 Base URL 和 API Key</small>
            </span>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 6 6-6 6" /></svg>
          </button>
        </div>
        <p>首次保存并使用 API 接入时，快速切换会自动开启。</p>
      </div>

      <div id="editor-progress" class="editor-progress" aria-label="设置进度" hidden>
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

      <div id="api-editor-body" class="editor-body" hidden>
        <div class="connection-fields">
          <label>
            <span>接入名称 <small>可选</small></span>
            <input id="connection-name" autocomplete="off" placeholder="例如：我的 Coding API" />
          </label>
          <label class="field-wide">
            <span>Base URL</span>
            <input id="base-url" type="url" autocomplete="url" placeholder="https://api.example.com/v1" />
            <small id="base-url-https-ip-warning" class="field-help field-warning" hidden></small>
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
          <label class="image-support-field">
            <input id="supports-images" type="checkbox" checked />
            <span>
              支持图片上传和截图
              <small>开启后 Codex 会显示贴图和截图。上游若不支持视觉，发送后可能失败。</small>
            </span>
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
    <p>重新打开一次 Codex 即可生效。之后保持本软件运行，新对话使用当前模型，已有对话仍走原来的模型。</p>
    <div class="restart-dialog-actions">
      <button id="restart-later" class="button button-secondary" type="button">稍后</button>
      <button id="restart-now" class="button button-primary" type="button">重新打开 Codex</button>
    </div>
  </dialog>

  <dialog id="update-notice" class="restart-dialog update-dialog" aria-labelledby="update-notice-title">
    <div class="restart-dialog-mark update-dialog-mark" aria-hidden="true">↑</div>
    <h2 id="update-notice-title">发现新版本</h2>
    <p id="update-notice-copy">发布页有可用更新。</p>
    <pre id="update-notice-notes" class="update-notes" hidden></pre>
    <div class="restart-dialog-actions">
      <button id="update-skip" class="button button-secondary" type="button">跳过此版本</button>
      <button id="update-now" class="button button-primary" type="button">立即更新</button>
    </div>
  </dialog>
`;

const status = required<HTMLOutputElement>("#status");

required<HTMLButtonElement>("#detect-outbound").addEventListener("click", () => {
  void refreshOutboundProxy(true);
});
required<HTMLButtonElement>("#diagnose-outbound").addEventListener("click", () => {
  void diagnoseOutbound(false);
});
required<HTMLButtonElement>("#ensure-outbound").addEventListener("click", () => {
  void diagnoseOutbound(true);
});
required<HTMLButtonElement>("#restart-codex").addEventListener("click", () => {
  void restartCodexHard();
});
required<HTMLButtonElement>("#refresh").addEventListener("click", refreshDashboard);
required<HTMLButtonElement>("#check-update").addEventListener("click", () => {
  void checkAppUpdate(true);
});
required<HTMLButtonElement>("#update-skip").addEventListener("click", () => {
  void skipPendingUpdate();
});
required<HTMLButtonElement>("#update-now").addEventListener("click", () => {
  void installPendingUpdate();
});
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
required<HTMLButtonElement>("#stop-proxy-main").addEventListener("click", disableLocalProxy);
required<HTMLButtonElement>("#repair-proxy").addEventListener("click", () => {
  void repairFastSwitch();
});
required<HTMLSelectElement>("#outbound-proxy-mode").addEventListener("change", (event) => {
  const target = event.currentTarget;
  if (!(target instanceof HTMLSelectElement)) return;
  void setOutboundProxyMode(target.value);
});
required<HTMLButtonElement>("#restore").addEventListener("click", restoreLatest);
required<HTMLButtonElement>("#open-codex").addEventListener("click", () => openCodex());
required<HTMLButtonElement>("#add-connection").addEventListener("click", openEditor);
required<HTMLElement>("#official-profile").addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof HTMLButtonElement)) return;
  if (target.dataset.officialAction === "login") void loginOfficialAccount();
  if (target.dataset.officialAction === "relogin") void reloginOfficialAccount();
  if (target.dataset.officialAction === "activate") void activateOfficial();
  if (target.dataset.officialAction === "logout") void logoutOfficialAccount();
});
required<HTMLButtonElement>("#choose-official-connection").addEventListener("click", async () => {
  await closeEditor();
  if (codexAccountAvailable && codexAccount.authMode === "chatgpt") {
    const proxyIsActive =
      localProxy.enabled && localProxy.running && !localProxy.recoveryRequired;
    if (isOfficialActive(proxyIsActive)) {
      setStatus("当前已使用这个 OpenAI 官方账号。", "success");
      return;
    }
    await activateOfficial();
    return;
  }
  await loginOfficialAccount();
});
required<HTMLButtonElement>("#choose-api-connection").addEventListener(
  "click",
  showApiConnectionEditor,
);
required<HTMLButtonElement>("#close-editor").addEventListener("click", closeEditor);
required<HTMLButtonElement>("#fetch-models").addEventListener("click", fetchAvailableModels);
required<HTMLButtonElement>("#toggle-key").addEventListener("click", toggleKeyVisibility);
required<HTMLInputElement>("#base-url").addEventListener("input", () => {
  updateHttpsLiteralIpWarnings();
  invalidateDiscovery();
});
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

void refreshDashboard().then(() => {
  showRequestedBrowserPreview();
  void checkAppUpdate(false);
});

function showRequestedBrowserPreview(): void {
  if (nativeAvailable) return;
  const preview = new URLSearchParams(window.location.search).get("preview");
  if (preview === "advanced") {
    setAdvancedSettingsVisible(true);
    return;
  }
  if (preview === "editor") {
    openEditor();
    showApiConnectionEditor();
    input("#connection-name").value = "Studio API";
    input("#base-url").value = "https://gateway.example.com/v1";
    input("#api-key").value = "demo-key";
    return;
  }
  if (preview === "connection-type") {
    openEditor();
    return;
  }
  if (preview === "access-token-conflict") {
    codexAccount = {
      ...browserAccountPreview,
      codexAccessTokenEnvironmentPresent: true,
    };
    renderDashboard();
    return;
  }
  if (preview === "update") {
    showUpdateDialog({
      currentVersion: "0.3.4",
      latestVersion: "0.3.5",
      updateAvailable: true,
      skipped: false,
      releaseNotes: "修复启动检测，并补充手动检查更新入口。",
      releaseUrl: "https://github.com/a182860089-pixel/codex-witcher-lm/releases/tag/v0.3.5",
      downloadUrl: "https://github.com/a182860089-pixel/codex-witcher-lm/releases/tag/v0.3.5",
      assetName: "Codex.Provider.Switcher_0.3.5_Windows-x64-Setup.exe",
    });
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
    : "";
  pageDescription.hidden = !advancedActive;
  required<HTMLElement>(".content-scroll").scrollTo({
    top: 0,
    behavior: "smooth",
  });
  const incoming = advancedActive ? panel : switcher;
  void animatePage(incoming);
}

async function refreshDashboard(): Promise<void> {
  if (!nativeAvailable) {
    renderDashboard();
    setStatus("通过桌面应用打开后，会自动读取当前接入和模型。", "info");
    return;
  }
  await run("正在读取当前状态…", async () => {
    await refreshCodexAccountStatus();
    dashboard = await invoke<DashboardState>("inspect_state");
    await refreshProxyStatus();
    await refreshOutboundProxy(false);
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
    if (!codexAccountAvailable) {
      return "已读取当前设置，但暂时无法确认 Codex 官方账号状态。";
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
  const builtInOpenAiRoute = isBuiltInOpenAiRoute(proxyIsActive);
  const accountUsesApiKey =
    builtInOpenAiRoute && codexAccountAvailable && codexAccount.authMode === "apiKey";
  const accessTokenEnvironmentConflict =
    builtInOpenAiRoute &&
    codexAccountAvailable &&
    codexAccount.codexAccessTokenEnvironmentPresent;
  const currentConnection = proxyIsActive
    ? (proxyProfile?.display_name ?? "已保存的接入")
    : officialIsActive
      ? (dashboard.officialProfile?.displayName ?? "OpenAI 官方账号")
      : accountUsesApiKey
        ? "OpenAI API Key"
        : accessTokenEnvironmentConflict
          ? "Codex 外部访问令牌"
        : dashboard.current.providerName;
  const currentModel = proxyIsActive
    ? (localProxy.currentModelId ?? "自动选择")
    : (dashboard.current.modelId ?? "自动选择");
  const currentEndpoint = proxyIsActive
    ? (proxyProfile?.base_url ?? "已保存的模型服务")
    : (dashboard.current.baseUrl ?? "OpenAI 官方服务");
  const currentAuth = proxyIsActive
    ? "系统密钥库 · 本机转发"
    : builtInOpenAiRoute
      ? codexAccountAuthLabel()
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
  } else if (accessTokenEnvironmentConflict) {
    currentBadge.textContent = "环境冲突";
    currentBadge.className = "badge badge-warning";
  } else if (accountUsesApiKey) {
    currentBadge.textContent = "OpenAI API Key";
    currentBadge.className = "badge badge-neutral";
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
  const builtInRoute = isBuiltInOpenAiRoute(proxyIsActive);
  const current = isOfficialActive(proxyIsActive);
  const loggedIn = codexAccountAvailable && codexAccount.authMode === "chatgpt";
  const accessTokenEnvironmentConflict =
    codexAccountAvailable && codexAccount.codexAccessTokenEnvironmentPresent;
  const blocked =
    busy ||
    dashboard.recoveryWarnings > 0 ||
    localProxy.manualRecoveryRequired ||
    localProxy.recoveryRequired;

  if (dashboard.officialProfileWarning) {
    badge.textContent = "需检查";
    badge.className = "badge badge-warning";
  } else if (!codexAccountAvailable) {
    badge.textContent = "状态不可用";
    badge.className = "badge badge-warning";
  } else if (accessTokenEnvironmentConflict) {
    badge.textContent = "环境冲突";
    badge.className = "badge badge-warning";
  } else if (current) {
    badge.textContent = "当前使用";
    badge.className = "badge badge-official";
  } else if (loggedIn) {
    badge.textContent = "已登录";
    badge.className = "badge badge-neutral";
  } else if (codexAccount.authMode === "apiKey") {
    badge.textContent = "OpenAI API Key";
    badge.className = "badge badge-neutral";
  } else {
    badge.textContent = codexAccount.authMode === "none" ? "未登录" : "其他认证";
    badge.className = "badge badge-neutral";
  }

  const explanation = officialAccountExplanation(current);
  const email = loggedIn
    ? (codexAccount.email ?? dashboard.officialProfile?.email ?? "Codex 未返回邮箱")
    : "登录后显示";
  const plan = loggedIn
    ? formatPlanType(codexAccount.planType ?? dashboard.officialProfile?.planType)
    : codexAccount.authMode === "apiKey"
      ? "OpenAI API Key"
      : "登录后显示";
  const accountMetadata = loggedIn
    ? `
      <dl class="official-account-meta">
        <div>
          <dt>账号邮箱</dt>
          <dd>${escapeHtml(email)}</dd>
        </div>
        <div>
          <dt>订阅</dt>
          <dd>${escapeHtml(plan)}</dd>
        </div>
      </dl>
    `
    : "";
  const accountActions = !loggedIn
    ? `<button class="button button-primary" data-official-action="login" type="button" ${blocked || dashboard.officialProfileWarning ? "disabled" : ""}>登录 OpenAI</button>`
    : !builtInRoute
      ? `
        <button class="button button-primary" data-official-action="activate" type="button" ${blocked || dashboard.officialProfileWarning ? "disabled" : ""}>使用此账号</button>
        <button class="button button-secondary" data-official-action="relogin" type="button" ${blocked ? "disabled" : ""}>登录其他账号</button>
      `
      : `
        <button class="button button-secondary" data-official-action="relogin" type="button" ${blocked ? "disabled" : ""}>重新登录</button>
        <button class="button button-quiet danger-text" data-official-action="logout" type="button" ${blocked ? "disabled" : ""}>退出账号</button>
      `;
  const environmentWarning = codexAccount.codexAccessTokenEnvironmentPresent
    ? `
      <p class="official-account-warning">
        检测到 CODEX_ACCESS_TOKEN。若 Codex 从同一环境启动，该外部访问令牌会优先于已保存的 ChatGPT 登录；清除后请完整退出并重新打开本软件和 Codex。
      </p>
    `
    : "";

  root.innerHTML = `
    <div class="official-main">
      <div class="official-copy">
        <p>${escapeHtml(explanation)}</p>
      </div>
      ${accountMetadata}
      ${environmentWarning}
    </div>
    <div class="official-actions">
      ${accountActions}
    </div>
  `;
}

function isOfficialActive(proxyIsActive = false): boolean {
  return (
    isBuiltInOpenAiRoute(proxyIsActive) &&
    codexAccountAvailable &&
    codexAccount.authMode === "chatgpt" &&
    !codexAccount.codexAccessTokenEnvironmentPresent
  );
}

function isBuiltInOpenAiRoute(proxyIsActive = false): boolean {
  return (
    !proxyIsActive &&
    dashboard.current.providerId === "openai" &&
    dashboard.current.baseUrl === null
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
    health.querySelector("strong")!.textContent = "待首次使用";
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
    summaryTitle.textContent = "快速切换将在首次使用时开启";
    summaryCopy.textContent = "首次保存并使用 API 接入时，应用会自动启用本机代理。";
    connectionsHelp.textContent = "添加或选择 API 接入和模型，快速切换会自动开启。";
    saveAndSwitch.textContent = "保存并使用";
    openCodexButton.hidden = true;
  }

  const issueText = localProxyIssueText();
  if (issueText) {
    summaryCopy.textContent = `${summaryCopy.textContent} ${issueText}`;
  }

  stopButton.hidden =
    !localProxy.enabled &&
    !localProxy.recoveryRequired &&
    !localProxy.manualRecoveryRequired &&
    !localProxy.lastError;
  stopButton.disabled = busy;
  const showRepairActions = needsProxyRepairActions();
  const proxyActions = document.querySelector<HTMLElement>("#current-proxy-actions");
  if (proxyActions) proxyActions.hidden = !showRepairActions;
  updateHttpsLiteralIpWarnings();
  if (
    manualRecoveryBlocked ||
    (switchMode === "localProxy" && localProxy.recoveryRequired)
  ) {
    saveAndSwitch.disabled = true;
  }
  renderOutboundNetwork();
}

function renderOutboundNetwork(): void {
  const health = document.querySelector<HTMLElement>("#network-health");
  const title = document.querySelector<HTMLElement>("#outbound-summary-title");
  const copy = document.querySelector<HTMLElement>("#outbound-summary-copy");
  const reportEl = document.querySelector<HTMLElement>("#outbound-report");
  const modeSelect = document.querySelector<HTMLSelectElement>("#outbound-proxy-mode");
  if (modeSelect && document.activeElement !== modeSelect) {
    modeSelect.value = localProxy.outboundProxyMode || "auto";
  }
  if (!health) return;

  health.className = "proxy-health";
  const strong = health.querySelector("strong");
  if (!outboundProxy) {
    health.classList.add("proxy-health-muted");
    if (strong) strong.textContent = "未检测";
  } else if (outboundProxy.detected && outboundProxy.listening) {
    health.classList.add("proxy-health-running");
    if (strong) strong.textContent = outboundProxy.proxyUrl ?? "已连接";
  } else if (outboundProxy.detected) {
    health.classList.add("proxy-health-warning");
    if (strong) strong.textContent = "已配置未监听";
  } else {
    health.classList.add("proxy-health-muted");
    if (strong) strong.textContent = "未发现";
  }

  if (title && copy) {
    if (outboundReport) {
      title.textContent = outboundReport.verdict;
      copy.textContent = outboundProxy?.detail ?? outboundReport.proxy.detail;
    } else if (outboundProxy) {
      title.textContent = outboundProxy.summary;
      copy.textContent = outboundProxy.detail;
    } else {
      title.textContent = "尚未检测";
      copy.textContent = "打开高级设置或点击检测后，会显示当前出站代理。";
    }
  }

  if (reportEl) {
    if (!outboundReport) {
      reportEl.hidden = true;
      reportEl.innerHTML = "";
    } else {
      reportEl.hidden = false;
      const via = outboundReport.viaProxy;
      const recs = outboundReport.recommendations
        .map((item) => "<li>" + escapeHtml(item) + "</li>")
        .join("");
      reportEl.innerHTML =
        '<div class="outbound-probe-grid">' +
        '<div><span>直连</span><strong class="' +
        (outboundReport.direct.ok ? "ok" : "bad") +
        '">' +
        escapeHtml(outboundReport.direct.message) +
        "</strong><small>" +
        outboundReport.direct.latencyMs +
        " ms</small></div>" +
        '<div><span>经代理</span><strong class="' +
        (via?.ok ? "ok" : "bad") +
        '">' +
        escapeHtml(via?.message ?? "无代理") +
        "</strong><small>" +
        (via ? via.latencyMs + " ms" : "-") +
        "</small></div></div>" +
        '<p class="outbound-probe-base">探测: ' +
        escapeHtml(outboundReport.probeBase) +
        "</p>" +
        (outboundReport.startedProxy
          ? '<p class="outbound-started">' + escapeHtml(outboundReport.startedProxy) + "</p>"
          : "") +
        (recs ? '<ul class="outbound-recs">' + recs + "</ul>" : "");
    }
  }

  const detectBtn = document.querySelector<HTMLButtonElement>("#detect-outbound");
  const diagBtn = document.querySelector<HTMLButtonElement>("#diagnose-outbound");
  const ensureBtn = document.querySelector<HTMLButtonElement>("#ensure-outbound");
  if (detectBtn) detectBtn.disabled = busy || !nativeAvailable;
  if (diagBtn) diagBtn.disabled = busy || !nativeAvailable;
  if (ensureBtn) ensureBtn.disabled = busy || !nativeAvailable;
  const restartCodexBtn = document.querySelector<HTMLButtonElement>("#restart-codex");
  if (restartCodexBtn) restartCodexBtn.disabled = busy || !nativeAvailable;
  const checkUpdateBtn = document.querySelector<HTMLButtonElement>("#check-update");
  if (checkUpdateBtn) checkUpdateBtn.disabled = busy || !nativeAvailable;
}

async function refreshOutboundProxy(userTriggered: boolean): Promise<void> {
  if (!nativeAvailable) return;
  try {
    if (userTriggered) setStatus("正在自动检测出站代理…", "working");
    outboundProxy = await invoke<OutboundProxyStatus>("detect_outbound_proxy");
    renderOutboundNetwork();
    if (userTriggered) {
      setStatus(
        outboundProxy.summary,
        outboundProxy.detected && outboundProxy.listening ? "success" : "info",
      );
    }
  } catch (error) {
    if (userTriggered) setStatus(String(error) || "出站代理检测失败", "error");
  }
}

async function diagnoseOutbound(tryStart: boolean): Promise<void> {
  if (!nativeAvailable) return;
  await run(
    tryStart ? "正在检测并尝试启动代理…" : "正在诊断上游网络（直连 vs 代理）…",
    async () => {
      const proxyProfile = dashboard.profiles.find(
        (p) => p.id === localProxy.currentProfileId,
      );
      const lumingProfile = dashboard.profiles.find(
        (p) => /luming/i.test(p.base_url) || /luming/i.test(p.display_name),
      );
      const activeBase =
        (localProxy.enabled && proxyProfile?.base_url) ||
        dashboard.current.baseUrl ||
        lumingProfile?.base_url ||
        "https://lumingapi.store";
      outboundReport = await invoke<OutboundNetworkReport>("diagnose_outbound_network", {
        probeBaseUrl: activeBase,
        tryStart,
      });
      outboundProxy = outboundReport.proxy;
      renderOutboundNetwork();
      return outboundReport.verdict;
    },
  );
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
  if (
    dashboard.recoveryWarnings > 0 ||
    dashboard.officialProfileWarning ||
    localProxy.manualRecoveryRequired
  ) {
    setStatus("配置或恢复记录需要人工处理；应用不会在修复前写入 Codex 配置。", "error");
    return;
  }
  await run("正在切换到 OpenAI 官方路由…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用切换官方账号配置。");
    try {
      await detachLocalProxyForOfficial();
      await invoke(
        dashboard.officialProfile ? "activate_official_profile" : "prepare_official_login",
      );
    } finally {
      await refreshOfficialState();
    }
    if (codexAccount.codexAccessTokenEnvironmentPresent) {
      throw new Error(
        "官方账号已登录，但检测到 CODEX_ACCESS_TOKEN。若 Codex 从同一环境启动，该外部访问令牌会优先于已保存的登录；请清除后完全退出并重新打开本软件和 Codex。",
      );
    }
    if (!isOfficialActive(false)) {
      throw new Error("官方路由已写入，但账号状态或 Codex 路由未能确认。");
    }
    return "已切换到 OpenAI 官方账号。请完全退出并重新打开 Codex。";
  });
}

async function loginOfficialAccount(): Promise<void> {
  if (
    dashboard.recoveryWarnings > 0 ||
    dashboard.officialProfileWarning ||
    localProxy.manualRecoveryRequired
  ) {
    setStatus("配置或恢复记录需要人工处理；应用不会在修复前开始登录。", "error");
    return;
  }
  await run("等待在浏览器中完成 OpenAI 登录…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用登录 OpenAI 官方账号。");
    try {
      await detachLocalProxyForOfficial();
      await invoke("prepare_official_login");
      await invoke<CodexAccountStatus>("login_official_account");
    } finally {
      await refreshOfficialState();
    }
    if (codexAccount.codexAccessTokenEnvironmentPresent) {
      throw new Error(
        "官方账号登录已保存，但检测到 CODEX_ACCESS_TOKEN。若 Codex 从同一环境启动，该外部访问令牌会优先于已保存的登录；请清除后完全退出并重新打开本软件和 Codex。",
      );
    }
    if (!isOfficialActive(false)) {
      throw new Error("登录已完成，但账号状态或 OpenAI 官方路由未能确认。");
    }
    const identity = codexAccount.email ?? "OpenAI 官方账号";
    return `已登录 ${identity}（${formatPlanType(codexAccount.planType)}）。请重新打开 Codex。`;
  });
}

async function reloginOfficialAccount(): Promise<void> {
  const currentIdentity =
    codexAccount.email ?? dashboard.officialProfile?.email ?? "当前官方账号";
  if (
    !window.confirm(
      `重新登录会替换 Codex 当前的 ${currentIdentity} 登录。Switcher 无法恢复旧账号；是否继续？`,
    )
  ) {
    return;
  }
  await loginOfficialAccount();
}

async function logoutOfficialAccount(): Promise<void> {
  if (
    !window.confirm(
      "退出 Codex 当前官方账号，并移除本软件保存的邮箱和订阅信息？",
    )
  ) {
    return;
  }
  await run("正在退出 OpenAI 官方账号…", async () => {
    if (!nativeAvailable) throw new Error("请通过桌面应用退出 OpenAI 官方账号。");
    try {
      await invoke<CodexAccountStatus>("logout_official_account");
    } finally {
      await refreshOfficialState();
    }
    if (codexAccount.authMode !== "none" || dashboard.officialProfile) {
      throw new Error("退出已执行，但账号状态或本地账号信息未能确认。");
    }
    return codexAccount.codexAccessTokenEnvironmentPresent
      ? "已退出已保存的 ChatGPT 登录并移除本地账号信息。启动环境仍有 Codex 外部访问令牌，请清除后重新打开本软件和 Codex。"
      : "已退出官方账号并移除本地账号信息。请完全退出并重新打开 Codex。";
  });
}

async function detachLocalProxyForOfficial(): Promise<void> {
  if (!localProxy.enabled && !localProxy.recoveryRequired) return;
  localProxy = await invokeProxyCommand("disable_proxy");
  if (localProxy.enabled || localProxy.running || localProxy.recoveryRequired) {
    throw new Error("快速切换尚未安全关闭，官方账号操作没有继续。");
  }
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
  required<HTMLInputElement>("#supports-images").checked = true;
  required<HTMLElement>("#model-step").hidden = true;
  required<HTMLElement>("#editor-kicker").textContent = "添加接入";
  required<HTMLElement>("#editor-title").textContent = "选择接入方式";
  required<HTMLElement>("#editor-description").textContent =
    "使用 OpenAI 官方账号，或连接兼容的 API 服务。";
  required<HTMLInputElement>("#api-key").placeholder = "输入 API Key";
  required<HTMLElement>("#api-key-help").textContent =
    "Key 只会保存在这台电脑的系统密钥库中。";
  required<HTMLButtonElement>("#fetch-models").textContent = "连接并获取模型";
  required<HTMLButtonElement>("#save-only").textContent = "仅保存";
  required<HTMLButtonElement>("#save-and-switch").textContent = "保存并使用";
  setEditorStep(1);
  setConnectionEditorView("choice");
  const editor = required<HTMLDialogElement>("#editor");
  if (!editor.open) {
    editor.showModal();
    void animateDialogIn(editor);
  }
  required<HTMLButtonElement>("#choose-official-connection").focus();
}

function showApiConnectionEditor(): void {
  setConnectionEditorView("api");
  required<HTMLElement>("#editor-title").textContent = "连接你的模型服务";
  required<HTMLElement>("#editor-description").textContent =
    "填写 Base URL 和 API Key，然后选择需要的模型。";
  required<HTMLInputElement>("#connection-name").focus();
}

function setConnectionEditorView(view: "choice" | "api"): void {
  const showChoice = view === "choice";
  required<HTMLElement>("#connection-type-step").hidden = !showChoice;
  required<HTMLElement>("#editor-progress").hidden = showChoice;
  required<HTMLElement>("#api-editor-body").hidden = showChoice;
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
  required<HTMLInputElement>("#supports-images").checked = profile.models.some(
    (model) => model.supports_images,
  );
  required<HTMLElement>("#model-step").hidden = false;
  setConnectionEditorView("api");
  required<HTMLElement>("#editor-description").textContent =
    "更新接入信息、API Key 或可用模型。";
  setEditorStep(2);
  const editor = required<HTMLDialogElement>("#editor");
  if (!editor.open) {
    editor.showModal();
    void animateDialogIn(editor);
  }
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
  if (editor.open) {
    await animateDialogOut(editor);
    editor.close();
  }
  required<HTMLElement>("#model-step").hidden = true;
  setEditorStep(1);
  input("#connection-name").value = "";
  input("#base-url").value = "";
  input("#api-key").value = "";
  input("#api-key").type = "password";
  required<HTMLButtonElement>("#toggle-key").textContent = "显示";
  input("#model-search").value = "";
  input("#manual-model").value = "";
  setConnectionEditorView("choice");
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

async function refreshCodexAccountStatus(): Promise<void> {
  try {
    codexAccount = await invoke<CodexAccountStatus>("codex_account_status");
    codexAccountAvailable = true;
  } catch {
    codexAccount = unavailableAccountStatus;
    codexAccountAvailable = false;
  }
}

async function refreshOfficialState(): Promise<void> {
  await refreshCodexAccountStatus();
  try {
    dashboard = await invoke<DashboardState>("inspect_state");
  } catch {
    // Preserve the last readable dashboard while still refreshing account/proxy state.
  }
  await refreshProxyStatus();
  renderDashboard();
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

async function repairFastSwitch(): Promise<void> {
  await run("\u6b63\u5728\u4e00\u952e\u4fee\u590d\u2026", async () => {
    const report = await invoke<RepairFastSwitchReport>("repair_fast_switch");
    localProxy = report.status;
    proxyApiAvailable = true;
    try {
      dashboard = await invoke<DashboardState>("inspect_state");
    } catch {
      // Keep the repaired proxy status even if Codex config refresh fails.
    }
    await refreshProxyStatus();
    renderDashboard();
    const steps = report.steps.filter(Boolean).join("\uFF1B");
    const issue = localProxyIssueText();
    if (localProxy.lastError || (localProxy.enabled && !localProxy.running)) {
      throw new Error(issue ? `${steps}\u3002${issue}` : steps);
    }
    return issue ? `${steps}\u3002${issue}` : steps || "\u4e00\u952e\u4fee\u590d\u5df2\u5b8c\u6210\u3002";
  });
}

async function setOutboundProxyMode(mode: string): Promise<void> {
  const normalized = mode.trim().toLowerCase();
  if (
    normalized !== "auto" &&
    normalized !== "direct" &&
    normalized !== "system"
  ) {
    return;
  }
  await run("\u6b63\u5728\u66f4\u65b0\u51fa\u7ad9\u4ee3\u7406\u65b9\u5f0f\u2026", async () => {
    localProxy = await invoke<LocalProxyStatus>("set_outbound_proxy_mode", {
      mode: normalized,
    });
    proxyApiAvailable = true;
    await refreshProxyStatus();
    renderDashboard();
    return `\u51fa\u7ad9\u65b9\u5f0f\u5df2\u8bbe\u4e3a${outboundProxyModeLabel(normalized)}\u3002`;
  });
}

function outboundProxyModeLabel(mode: string): string {
  if (mode === "direct") return "\u59cb\u7ec8\u76f4\u8fde";
  if (mode === "system") return "\u59cb\u7ec8\u8d70\u7cfb\u7edf\u4ee3\u7406";
  return "\u81ea\u52a8";
}

function needsProxyRepairActions(): boolean {
  if (!proxyApiAvailable) return false;
  return (
    dashboard.recoveryWarnings > 0 ||
    localProxy.manualRecoveryRequired ||
    localProxy.recoveryRequired ||
    Boolean(localProxy.lastError) ||
    (localProxy.enabled && !localProxy.running)
  );
}

function localProxyIssueText(): string {
  const parts: string[] = [];
  if (localProxy.lastError) parts.push(localProxy.lastError);
  if (
    localProxy.enabled &&
    !localProxy.running &&
    !(localProxy.lastError || "").includes("15722 \u672a\u76d1\u542c")
  ) {
    parts.push("15722 \u672a\u76d1\u542c");
  }
  if (localProxy.ccSwitchDetected) {
    parts.push("\u68c0\u6d4b\u5230 cc-switch\uff0c\u8bf7\u4e0d\u8981\u540c\u65f6\u5f00\u542f\u4e24\u4e2a\u5207\u6362\u5668\u3002");
  }
  return parts.join("\uFF1B");
}

function isHttpsLiteralIpUrl(value: string): boolean {
  try {
    const url = new URL(value.trim());
    if (url.protocol !== "https:") return false;
    const host = url.hostname.replace(/^\[/, "").replace(/\]$/, "");
    if (/^\d{1,3}(?:\.\d{1,3}){3}$/.test(host)) return true;
    return host.includes(":");
  } catch {
    return false;
  }
}

function updateHttpsLiteralIpWarnings(): void {
  const message =
    "HTTPS \u5b57\u9762 IP \u7684\u8bc1\u4e66\u901a\u5e38\u65e0\u6cd5\u901a\u8fc7\u6d4f\u89c8\u5668\u6821\u9a8c\uff1b\u8bf7\u7528 API \u63a2\u6d4b\u5065\u5eb7\u72b6\u6001\uff0c\u4e0d\u8981\u628a /usage \u9875\u9762\u5f53\u6210\u63a5\u53e3\u662f\u5426\u53ef\u7528\u7684\u4f9d\u636e\u3002";
  const editorWarning = document.querySelector<HTMLElement>("#base-url-https-ip-warning");
  if (editorWarning) {
    const show = isHttpsLiteralIpUrl(input("#base-url").value);
    editorWarning.hidden = !show;
    editorWarning.textContent = show ? message : "";
  }
  const currentWarning = document.querySelector<HTMLElement>("#current-https-ip-warning");
  if (currentWarning) {
    const proxyIsActive =
      localProxy.enabled && localProxy.running && !localProxy.recoveryRequired;
    const proxyProfile = dashboard.profiles.find(
      (profile) => profile.id === localProxy.currentProfileId,
    );
    const endpoint = proxyIsActive
      ? (proxyProfile?.base_url ?? "")
      : (dashboard.current.baseUrl ?? "");
    const show = isHttpsLiteralIpUrl(endpoint);
    currentWarning.hidden = !show;
    currentWarning.textContent = show ? message : "";
  }
}

async function disableLocalProxy(): Promise<void> {
  if (
    !localProxy.enabled &&
    !localProxy.recoveryRequired &&
    !localProxy.manualRecoveryRequired &&
    !localProxy.lastError
  ) {
    return;
  }
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

async function restartCodexHard(): Promise<void> {
  if (!nativeAvailable) {
    setStatus("请在桌面应用中使用一键重启 Codex。", "error");
    return;
  }
  if (
    !window.confirm(
      "将强制结束 Codex 主进程及其 OpenAI\\Codex 运行时（含卡住的进程），然后重新打开。未保存的对话可能丢失。继续？",
    )
  ) {
    return;
  }
  await run("正在强制结束并重启 Codex…", async () => {
    const detail = await invoke<string>("restart_codex");
    await refreshProxyStatus();
    await refreshOutboundProxy(false).catch(() => undefined);
    renderDashboard();
    return `Codex 已强制重启（${detail}）。`;
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
        supports_images: required<HTMLInputElement>("#supports-images").checked,
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

function officialAccountExplanation(current: boolean): string {
  if (!codexAccountAvailable) {
    return "暂时无法确认 Codex 账号状态。可以刷新状态，或重新发起官方登录。";
  }
  if (codexAccount.codexAccessTokenEnvironmentPresent) {
    return codexAccount.authMode === "chatgpt"
      ? "官方账号登录已保存，但启动环境中存在 Codex 外部访问令牌。"
      : "检测到 Codex 外部访问令牌。清除后再登录 OpenAI 官方账号。";
  }
  if (codexAccount.authMode === "apiKey") {
    return "Codex 当前使用 OpenAI API Key。登录官方账号后会改用 ChatGPT 账号认证。";
  }
  if (codexAccount.authMode === "other") {
    return "Codex 当前使用其他认证方式。可重新登录 OpenAI 官方账号。";
  }
  if (codexAccount.authMode === "none") {
    return "尚未登录 OpenAI 官方账号。登录将在浏览器中完成。";
  }
  return current
    ? "OpenAI 官方账号已登录，Codex 当前使用内置 OpenAI 路由。"
    : "OpenAI 官方账号已登录；切换官方路由并重新打开 Codex 后使用。";
}

function formatPlanType(planType: string | null | undefined): string {
  if (!planType) return "Codex 未返回套餐";
  const knownPlans: Record<string, string> = {
    free: "Free",
    plus: "Plus",
    pro: "Pro",
    team: "Team",
    business: "Business",
    enterprise: "Enterprise",
    edu: "Edu",
  };
  return knownPlans[planType.toLocaleLowerCase()] ?? planType;
}

function codexAccountAuthLabel(): string {
  if (!codexAccountAvailable) return "Codex 账号状态不可用";
  if (codexAccount.codexAccessTokenEnvironmentPresent) {
    return "Codex 外部访问令牌";
  }
  return (
    {
      none: "尚未登录",
      apiKey: "OpenAI API Key",
      chatgpt: "ChatGPT 官方登录",
      other: "其他 Codex 认证",
    } satisfies Record<CodexAuthMode, string>
  )[codexAccount.authMode];
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

async function checkAppUpdate(manual: boolean): Promise<void> {
  if (!nativeAvailable) {
    if (manual) {
      setStatus("请通过桌面应用检查更新。", "info");
    }
    return;
  }
  if (manual) {
    setBusy(true);
    setStatus("正在检查更新…", "working");
  }
  try {
    const status = await invoke<AppUpdateStatus>("check_app_update");
    required<HTMLElement>("#app-version").textContent = `Version ${status.currentVersion}`;
    if (!status.updateAvailable) {
      if (manual) {
        setStatus(`当前已是最新版本 ${status.currentVersion}`, "success");
      }
      return;
    }
    if (!manual && status.skipped) return;
    showUpdateDialog(status);
    if (manual) {
      setStatus(`发现新版本 ${status.latestVersion}`, "success");
    }
  } catch (error) {
    if (manual) setStatus(friendlyError(error), "error");
  } finally {
    if (manual) setBusy(false);
  }
}

function showUpdateDialog(status: AppUpdateStatus): void {
  pendingUpdate = status;
  required<HTMLElement>("#update-notice-copy").textContent =
    `当前 Version ${status.currentVersion}，可更新到 ${status.latestVersion}。将在当前安装目录静默覆盖，无需卸载重装。`;
  const notes = required<HTMLElement>("#update-notice-notes");
  const body = status.releaseNotes.trim();
  notes.hidden = !body;
  notes.textContent = body;
  const dialog = required<HTMLDialogElement>("#update-notice");
  if (!dialog.open) dialog.showModal();
}

async function skipPendingUpdate(): Promise<void> {
  const status = pendingUpdate;
  required<HTMLDialogElement>("#update-notice").close();
  if (!status) return;
  if (!nativeAvailable) {
    setStatus(`已跳过 ${status.latestVersion}`, "info");
    return;
  }
  await run(`已跳过 ${status.latestVersion}`, async () => {
    await invoke("skip_app_update", { version: status.latestVersion });
    return `已跳过 ${status.latestVersion}，有更新版本时会再提醒。`;
  });
}

async function installPendingUpdate(): Promise<void> {
  const status = pendingUpdate;
  if (!status) return;
  required<HTMLDialogElement>("#update-notice").close();
  const url = status.downloadUrl || status.releaseUrl;
  if (!nativeAvailable) {
    window.open(url, "_blank", "noopener");
    return;
  }
  const canInstall =
    Boolean(status.assetName) && status.downloadUrl.includes("/releases/download/");
  if (!canInstall) {
    await run("正在打开更新发布页…", async () => {
      await invoke("open_app_update", { url: status.releaseUrl || url });
      return `已打开 ${status.latestVersion} 的发布页。`;
    });
    return;
  }
  await run("正在下载并安装更新…", async () => {
    await invoke("install_app_update", {
      url: status.downloadUrl,
      assetName: status.assetName,
    });
    return `正在安装 ${status.latestVersion}，完成后会自动重新打开。`;
  });
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
  if (
    message.includes("could not check for updates") ||
    message.includes("update request timed out") ||
    message.includes("update endpoint") ||
    message.includes("update response")
  ) {
    return "暂时无法检查更新，请检查网络后重试。";
  }
  if (message.includes("could not open the update page") || message.includes("update URL")) {
    return "无法打开更新链接。请稍后重试，或到 GitHub Release 页面手动下载。";
  }
  if (
    message.includes("could not download the installer") ||
    message.includes("could not launch the installer") ||
    message.includes("could not stage the installer") ||
    message.includes("update download") ||
    message.includes("downloaded installer")
  ) {
    return "无法完成应用内更新。请检查网络后重试，或到 GitHub Release 页面手动下载。";
  }
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
    return "请先切换到 OpenAI 官方路由并完成登录；账号信息会在登录后自动保存。";
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
