import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
  hiddenAliasCount?: number;
}

interface CredentialSessionSummary {
  sessionId: string;
  baseUrl: string;
}

interface ProfileEditorOptions {
  credentialMissing?: boolean;
}

type SwitchMode = "localProxy" | "directConfig";
type AppPage = "dashboard" | "switcher" | "inspector" | "advanced";
type UsageRange = "hour" | "day" | "minutes10" | "week" | "month" | "custom";

interface UsageSeriesPoint {
  label: string;
  startMs: number;
  cachedTokens: number;
  uncachedTokens: number;
  cacheWriteTokens: number;
  completionTokens: number;
  totalTokens: number;
  calls: number;
}

interface UsageHeatCell {
  date: string;
  weekday: number;
  totalTokens: number;
  calls: number;
}

interface HeatMonthLabel {
  label: string;
  column: number;
}

interface UsageOverview {
  range: string;
  fromMs: number;
  toMs: number;
  calls: number;
  success: number;
  errors: number;
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  cacheWriteTokens?: number;
  totalTokens: number;
  cacheHitRate: number;
  estimatedUsd: number;
  cacheUsd: number;
  series: UsageSeriesPoint[];
  heatmap: UsageHeatCell[];
  heatMonths: HeatMonthLabel[];
}

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
  upstreamMaxRetries: number;
  upstreamRetryMaxElapsedSeconds: number;
}

interface RepairFastSwitchReport {
  steps: string[];
  status: LocalProxyStatus;
}

interface ToolChannelDiagnosis {
  healthy: boolean;
  platformSupported: boolean;
  setupErrorCode: string | null;
  setupErrorMessage: string | null;
  agentMode: string | null;
  guardianModeActive: boolean;
  sandboxAclFailures: string[];
  issues: string[];
  recommendations: string[];
}

interface ToolChannelRepairReport {
  steps: string[];
  diagnosis: ToolChannelDiagnosis;
  requiresCodexRestart: boolean;
  elevationAttempted: boolean;
  elevationNeeded: boolean;
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

const CONTEXT_WINDOW_STEPS = [64_000, 128_000, 250_000, 500_000, 1_000_000, 2_000_000];
const DEFAULT_CONTEXT_WINDOW = 250_000;

const previewModel = (
  id: string,
  displayName: string,
  description: string,
): ModelSpec => ({
  id,
  display_name: displayName,
  description,
  context_window: DEFAULT_CONTEXT_WINDOW,
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
  upstreamMaxRetries: 8,
  upstreamRetryMaxElapsedSeconds: 90,
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
let toolChannel: ToolChannelDiagnosis | null = null;
let localProxy = stoppedProxy;
let outboundProxy: OutboundProxyStatus | null = null;
let outboundReport: OutboundNetworkReport | null = null;
let codexAccount = browserAccountPreview;
let codexAccountAvailable = !nativeAvailable;
let switchMode: SwitchMode = "localProxy";

interface RequestLogItem {
  id: string;
  time: string;
  provider: string;
  model: string;
  displayName?: string;
  endpoint: string;
  status: number;
  durationMs: number;
  threadId?: string;
  error?: string;
  details?: string;
  retryCount?: number;
  firstByteMs?: number;
  responseBytes?: number;
  streamDurationMs?: number;
  streamCompleted?: boolean;
  streamError?: string;
  requestedModel?: string;
  agentGuard?: string;
  completedWithoutTools?: boolean;
  agentNudged?: boolean;
  startedAtMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  cachedTokens?: number;
  cacheWriteTokens?: number;
  totalTokens?: number;
  reasoningEffort?: string;
  finishReason?: string;
  serviceTier?: string;
}

let usageRange: UsageRange = "month";
let usageOverview: UsageOverview = {
  range: "month",
  fromMs: 0,
  toMs: 0,
  calls: 0,
  success: 0,
  errors: 0,
  promptTokens: 0,
  completionTokens: 0,
  cachedTokens: 0,
  cacheWriteTokens: 0,
  totalTokens: 0,
  cacheHitRate: 0,
  estimatedUsd: 0,
  cacheUsd: 0,
  series: [],
  heatmap: [],
  heatMonths: [],
};
let usageFetchToken = 0;

let requestLogs: RequestLogItem[] = previewRequestLogs();

let selectedLogId: string | null = null;
let logFilterStatus: "all" | "success" | "error" = "all";
let logPageIndex = 1;
let logPageSize = 20;

let activePage: AppPage = "switcher";
let discoveryId: string | null = null;
let discoveredModels: FetchedModel[] = [];
let hiddenAliasCount = 0;
let selectedModels = new Set<string>();
let editingProfileId: string | null = null;
let editingCredentialLoaded = false;
let editingCredentialDirty = false;
let restartNoticeShown = false;
let restartNoticePresenting = false;
let updateNoticePresenting = false;
let mainWindowShown = !nativeAvailable;
let busy = false;
let pendingUpdate: AppUpdateStatus | null = null;
const AUTO_UPDATE_FIRST_DELAY_MS = 5000;
const AUTO_UPDATE_INTERVAL_MS = 4 * 60 * 60 * 1000;
const AUTO_UPDATE_RETRY_DELAYS_MS = [30_000, 120_000, 600_000];
let autoUpdateTimer: number | null = null;
let autoUpdateInFlight = false;
let autoUpdateRetryIndex = 0;

app.innerHTML = `
  <div class="app-shell">
    <aside class="sidebar" aria-label="应用导航">
      <div class="brand">
        <div class="brand-mark" aria-hidden="true">
          <img src="/icon.png" alt="" />
        </div>
        <div class="brand-copy">
          <strong>LM Codex Switch</strong>
        </div>
      </div>

      <nav class="sidebar-nav">
        <div class="nav-group">
          <p class="nav-group-label">概览</p>
          <button id="nav-dashboard" class="nav-item" type="button" aria-label="打开仪表盘">
            <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="3" width="7" height="9" rx="1.5" /><rect x="14" y="3" width="7" height="5" rx="1.5" /><rect x="14" y="12" width="7" height="9" rx="1.5" /><rect x="3" y="16" width="7" height="5" rx="1.5" /></svg>
            <span>仪表盘</span>
          </button>
          <button id="nav-switcher" class="nav-item nav-item-active" type="button" aria-current="page">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 7.5h16M4 16.5h16M8 4v7M16 13v7" />
            </svg>
            <span>模型切换</span>
          </button>
        </div>
        <div class="nav-group">
          <p class="nav-group-label">工具</p>
          <button id="nav-inspector" class="nav-item" type="button" aria-label="打开调用详情">
            <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="7" /><line x1="21" y1="21" x2="16.65" y2="16.65" /><line x1="8" y1="11" x2="14" y2="11" /></svg>
            <span>调用详情</span>
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
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
            </svg>
            <span>高级设置</span>
          </button>
        </div>
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
        <div class="sidebar-footer-row">
          <button id="check-update" class="check-update-btn" type="button" title="检查应用更新">
            <span>检查更新</span>
            <small id="app-version">Version</small>
          </button>
          <button id="theme-toggle-btn" class="theme-toggle-btn" type="button" aria-label="切换浅色/深色主题" title="切换浅色/深色外观">
            <svg class="icon-sun" viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="12" cy="12" r="4" fill="none" stroke="currentColor" stroke-width="2"/>
              <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
            </svg>
            <svg class="icon-moon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          </button>
        </div>
      </div>
    </aside>

    <div class="workspace">
      <main class="content-scroll">
        
        <section id="dashboard-view" class="page-view dashboard-view" aria-labelledby="db-heading" hidden>
          <div class="usage-head">
            <h2 id="db-heading">概览</h2>
            <div class="usage-toolbar">
              <div class="usage-range" role="tablist" aria-label="用量时间范围">
                <button class="usage-range-btn" type="button" data-usage-range="hour" role="tab">近1小时</button>
                <button class="usage-range-btn" type="button" data-usage-range="day" role="tab">近1自然日</button>
                <button class="usage-range-btn" type="button" data-usage-range="minutes10" role="tab">近10分钟</button>
                <button class="usage-range-btn" type="button" data-usage-range="week" role="tab">近一周</button>
                <button class="usage-range-btn is-active" type="button" data-usage-range="month" role="tab" aria-selected="true">近一个月</button>
                <button class="usage-range-btn" type="button" data-usage-range="custom" role="tab">自定义</button>
              </div>
              <button id="usage-refresh" class="usage-refresh" type="button" aria-label="刷新用量" title="刷新用量">
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M20 11a8 8 0 1 0-2.34 5.66M20 5v6h-6" />
                </svg>
              </button>
            </div>
          </div>
          <div id="usage-custom" class="usage-custom" hidden>
            <label>
              <span>开始</span>
              <input id="usage-from" type="date" />
            </label>
            <label>
              <span>结束</span>
              <input id="usage-to" type="date" />
            </label>
            <button id="usage-custom-apply" class="button button-secondary" type="button">应用</button>
          </div>
          <section class="usage-chart-card" aria-label="用量趋势">
            <div id="usage-chart" class="usage-chart"></div>
          </section>
          <section class="usage-kpi-grid" aria-label="用量指标">
            <article class="usage-kpi">
              <div class="usage-kpi-label">
                <span>缓存命中率</span>
                <span class="usage-info" title="缓存输入 Token 占提示词 Token 的比例">i</span>
              </div>
              <div class="usage-gauge">
                <svg viewBox="0 0 120 120" aria-hidden="true">
                  <circle class="usage-gauge-track" cx="60" cy="60" r="46" />
                  <circle id="usage-hit-ring" class="usage-gauge-value" cx="60" cy="60" r="46" />
                </svg>
                <strong id="usage-hit-rate">0%</strong>
              </div>
            </article>
            <article class="usage-kpi">
              <div class="usage-kpi-label">
                <span>LLM 调用</span>
                <span class="usage-info" title="本地代理转发的模型请求次数">i</span>
              </div>
              <strong id="usage-calls">0</strong>
              <small id="usage-calls-sub">成功 0 / 异常 0</small>
            </article>
            <article class="usage-kpi">
              <div class="usage-kpi-label">
                <span>Token 消耗</span>
                <span class="usage-info" title="提示词与模型输出 Token 合计">i</span>
              </div>
              <strong id="usage-tokens">0</strong>
              <small id="usage-tokens-sub">提示词 0</small>
            </article>
            <article class="usage-kpi">
              <div class="usage-kpi-label">
                <span>价值估算</span>
                <span class="usage-info" title="按公开单价估算，仅供参考">i</span>
              </div>
              <strong id="usage-cost">$0.00</strong>
              <small id="usage-cost-sub">缓存读写 $0.00</small>
            </article>
          </section>
          <section class="usage-heat-card" aria-label="年度用量热力图">
            <div class="usage-heat-scroll">
              <div id="usage-heat-months" class="usage-heat-months"></div>
              <div id="usage-heatmap" class="usage-heatmap"></div>
            </div>
          </section>
          <div id="usage-tooltip" class="usage-tooltip" hidden></div>
        </section>

        <section id="inspector-view" class="page-view inspector-view" aria-labelledby="insp-heading" hidden>
          <div class="inspector-header-bar">
            <div class="inspector-title-group">
              <h2 id="insp-heading" class="inspector-title">调用</h2>
              <div class="inspector-filters" role="tablist" aria-label="按状态筛选">
                <button id="insp-filter-all" class="filter-chip chip-active" type="button">全部</button>
                <button id="insp-filter-success" class="filter-chip" type="button">成功</button>
                <button id="insp-filter-error" class="filter-chip" type="button">异常</button>
              </div>
            </div>
            <div class="inspector-actions">
              <button id="insp-mock-ping" class="button button-quiet" type="button">测速</button>
              <button id="insp-clear-logs" class="button button-quiet" type="button">清空</button>
              <button id="insp-refresh" class="button button-icon" type="button" aria-label="刷新调用记录" title="刷新">
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M20 11a8 8 0 1 0-2.34 5.66M20 5v6h-6" />
                </svg>
              </button>
            </div>
          </div>

          <div class="inspector-table-card">
            <div class="inspector-table-container">
              <table class="inspector-table">
                <thead>
                  <tr>
                    <th>状态</th>
                    <th>显示名称</th>
                    <th>时间</th>
                    <th>模型名称</th>
                    <th>思考强度</th>
                    <th>Fast</th>
                    <th>调用类型</th>
                    <th>路由</th>
                    <th>Finish Reason</th>
                    <th>HTTP</th>
                    <th>耗时</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody id="inspector-log-tbody">
                </tbody>
              </table>
            </div>
            <div class="inspector-pager" id="inspector-pager"></div>
          </div>
        </section>

        <section id="switcher-page" class="page-view" aria-labelledby="current-title">
          <section class="settings-block">
            <h2 class="list-section-title">当前使用</h2>
            <section class="current-section grouped-list">
              <div class="list-row current-heading">
                <div class="current-symbol" aria-hidden="true">
                  <svg viewBox="0 0 24 24">
                    <path d="m8 7 5 5-5 5M13 7l5 5-5 5" />
                  </svg>
                </div>
                <div class="current-copy list-copy">
                  <h2 id="current-title">正在读取…</h2>
                  <p id="current-kicker">当前模型</p>
                </div>
                <span id="current-badge" class="badge">读取中</span>
              </div>
              <div id="current-details" class="current-details"></div>
              <p id="current-https-ip-warning" class="current-https-ip-warning" hidden></p>
              <div id="current-proxy-actions" class="current-proxy-actions list-row-actions" hidden>
                <button id="repair-proxy" class="button button-primary" type="button">一键修复</button>
                <button id="stop-proxy-main" class="button button-quiet danger-text" type="button">关闭快速切换</button>
              </div>
            </section>
          </section>

          <section class="connections-section settings-block" aria-labelledby="connections-title">
            <div class="section-title-row">
              <div>
                <h2 id="connections-title" class="list-section-title">接入</h2>
                <p id="connections-help">选择一项即可切换。</p>
              </div>
              <button id="add-connection" class="button button-quiet" type="button">
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M12 5v14M5 12h14" />
                </svg>
                <span>添加接入</span>
              </button>
            </div>

            <div class="provider-list grouped-list">
              <article class="official-section" aria-labelledby="official-title">
                <div id="official-profile"></div>
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
            <button id="advanced-settings-close" class="button button-quiet" type="button">
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

          <section class="settings-group" aria-labelledby="retry-heading">
            <div class="settings-group-heading">
              <h3 id="retry-heading">上游失败重试</h3>
              <p>只重试连接失败、超时、429、502、503、504；一旦收到上游响应并开始流式输出，断流不会盲目重放请求。</p>
            </div>
            <div class="mode-summary">
              <div>
                <strong id="retry-summary-title">最多重试 8 次</strong>
                <p id="retry-summary-copy">单个请求的重试总等待时间上限为 90 秒。</p>
              </div>
              <div class="mode-summary-actions retry-settings-actions">
                <label class="outbound-mode-field">
                  <span>最大重试次数</span>
                  <input id="upstream-max-retries" type="number" min="0" max="20" step="1" value="8" />
                </label>
                <button id="save-retry-settings" class="button button-primary" type="button">应用重试设置</button>
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
                <button id="restart-codex" class="button button-secondary" type="button">重启 Codex</button>
                <button id="open-codex" class="button button-secondary" type="button" hidden>重新打开 Codex</button>
                <button id="stop-proxy" class="button button-secondary danger-text" type="button" hidden>关闭快速切换</button>
              </div>
            </div>
          </section>

          <section class="settings-group" aria-labelledby="tool-channel-heading">
            <div class="settings-group-heading">
              <h3 id="tool-channel-heading">Codex 工具通道</h3>
              <p>修复 Windows 沙箱 ACL / Guardian 模式导致的 shell、Node、MCP 全挂。升级后可在此一键处理。</p>
            </div>
            <div class="mode-summary">
              <div>
                <strong id="tool-channel-title">尚未检测</strong>
                <p id="tool-channel-copy">打开高级设置后会自动检测本机 Codex 沙箱与权限模式。</p>
                <ul id="tool-channel-issues" class="tool-channel-issues" hidden></ul>
              </div>
              <div class="mode-summary-actions">
                <button id="diagnose-tool-channel" class="button button-secondary" type="button">重新检测</button>
                <button id="repair-tool-channel" class="button button-primary" type="button">一键修复工具通道</button>
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
              <p>以后快速切换时，只显示这些模型。优先勾选 grok-4.6 这种短 ID。</p>
            </div>
            <strong id="selected-count">已选择 0 个</strong>
          </div>
          <p id="model-alias-hint" class="model-alias-hint" hidden>
            已自动隐藏带供应商前缀的别名。请选择 grok-4.6，不要选 x-ai/grok-4.6。
          </p>
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
          <div class="context-window-field">
            <div class="context-window-header">
              <label for="context-window">上下文上限</label>
              <strong id="context-window-value">250k</strong>
            </div>
            <input
              id="context-window"
              type="range"
              min="0"
              max="5"
              step="1"
              value="2"
              aria-valuemin="0"
              aria-valuemax="5"
              aria-valuenow="2"
              aria-valuetext="250k"
            />
            <div class="context-window-ticks" aria-hidden="true">
              <span>64k</span>
              <span>128k</span>
              <span>250k</span>
              <span>500k</span>
              <span>1M</span>
              <span>2M</span>
            </div>
            <small>Codex 会按这个上限的 95% 自动压缩。改完后重新打开 Codex，新对话才会用上。不要超过上游模型的真实窗口。</small>
          </div>
          <div class="editor-actions">
            <button id="save-only" class="button button-secondary" type="button">仅保存</button>
            <button id="save-and-switch" class="button button-primary" type="button">保存并使用</button>
          </div>
        </div>
      </div>
    </div>
  </dialog>

  <dialog id="restart-notice" class="notice-dialog" aria-labelledby="restart-notice-title">
    <div class="restart-dialog">
      <div class="restart-dialog-mark" aria-hidden="true">✓</div>
      <h2 id="restart-notice-title">快速切换已准备好</h2>
      <p>重新打开一次 Codex 即可生效。之后保持本软件运行，新对话使用当前模型，已有对话仍走原来的模型。</p>
      <div class="restart-dialog-actions">
        <button id="restart-later" class="button button-secondary" type="button" autofocus>稍后</button>
        <button id="restart-now" class="button button-primary" type="button">重新打开 Codex</button>
      </div>
    </div>
  </dialog>

  <dialog id="update-notice" class="notice-dialog" aria-labelledby="update-notice-title">
    <div class="restart-dialog update-dialog">
      <div class="restart-dialog-mark update-dialog-mark" aria-hidden="true">↑</div>
      <h2 id="update-notice-title">发现新版本</h2>
      <p id="update-notice-copy">发布页有可用更新。</p>
      <pre id="update-notice-notes" class="update-notes" hidden></pre>
      <div class="restart-dialog-actions">
        <button id="update-skip" class="button button-secondary" type="button">跳过此版本</button>
        <button id="update-now" class="button button-primary" type="button" autofocus>立即更新</button>
      </div>
    </div>
  </dialog>

  <dialog id="inspector-detail" class="notice-dialog inspector-detail-dialog" aria-labelledby="inspector-detail-title">
    <div class="inspector-detail-sheet">
      <header class="inspector-detail-head">
        <div>
          <p class="section-kicker">请求详情</p>
          <h2 id="inspector-detail-title">调用记录</h2>
          <p id="inspector-detail-id" class="panel-sub-id">未选中请求</p>
        </div>
        <button id="inspector-detail-close" class="button button-icon" type="button" aria-label="关闭">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m7 7 10 10M17 7 7 17" />
          </svg>
        </button>
      </header>
      <div id="inspector-detail-content" class="inspector-detail-body">
        <div class="detail-empty-placeholder">
          <p>选择一条请求，查看元数据与报文。</p>
        </div>
      </div>
    </div>
  </dialog>
`;

const status = required<HTMLOutputElement>("#status");


// --- 主题切换支持 (Light / Dark Theme) ---
type ThemeMode = "light" | "dark" | "system";
const THEME_STORAGE_KEY = "codex_theme_preference";

function applyTheme(theme: ThemeMode): void {
  const root = document.documentElement;
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
  localStorage.setItem(THEME_STORAGE_KEY, theme);
  updateThemeButtonUI(theme);
}

function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem(THEME_STORAGE_KEY);
  if (stored === "light" || stored === "dark") return stored;
  return "system";
}

function updateThemeButtonUI(theme: ThemeMode): void {
  const btn = document.querySelector<HTMLButtonElement>("#theme-toggle-btn");
  if (!btn) return;
  const isDark = theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  btn.setAttribute("data-active-theme", isDark ? "dark" : "light");
  btn.title = isDark ? "当前深色，点击切换至浅色模式" : "当前浅色，点击切换至深色模式";
}

function initTheme(): void {
  const currentTheme = getStoredTheme();
  applyTheme(currentTheme);

  const btn = document.querySelector<HTMLButtonElement>("#theme-toggle-btn");
  btn?.addEventListener("click", () => {
    const isDarkNow = document.documentElement.getAttribute("data-theme") === "dark" ||
      (!document.documentElement.getAttribute("data-theme") && window.matchMedia("(prefers-color-scheme: dark)").matches);
    const nextTheme: ThemeMode = isDarkNow ? "light" : "dark";
    applyTheme(nextTheme);
  });

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (!document.documentElement.getAttribute("data-theme")) {
      updateThemeButtonUI("system");
    }
  });
}

initTheme();

const MORE_ICON =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="12" r="1.4" /><circle cx="12" cy="12" r="1.4" /><circle cx="18" cy="12" r="1.4" /></svg>';

function closeAllMenus(except?: HTMLElement): void {
  document.querySelectorAll<HTMLElement>(".row-menu").forEach((menu) => {
    if (menu === except) return;
    menu.hidden = true;
    menu.style.top = "";
    menu.style.bottom = "";
    const owner = menu
      .closest(".row-more")
      ?.querySelector<HTMLElement>("[aria-expanded]");
    owner?.setAttribute("aria-expanded", "false");
  });
}

function toggleMenu(button: HTMLElement, menu: HTMLElement): void {
  const willOpen = menu.hidden;
  closeAllMenus(willOpen ? menu : undefined);
  menu.hidden = !willOpen;
  button.setAttribute("aria-expanded", String(willOpen));
  if (willOpen) positionOverflowMenu(menu);
}

function positionOverflowMenu(menu: HTMLElement): void {
  menu.style.top = "";
  menu.style.bottom = "";
  const rect = menu.getBoundingClientRect();
  const scroller = document.querySelector<HTMLElement>(".content-scroll");
  const scrollerBottom = scroller?.getBoundingClientRect().bottom ?? window.innerHeight;
  if (rect.bottom > scrollerBottom - 8) {
    menu.style.top = "auto";
    menu.style.bottom = "calc(100% + 6px)";
  }
}

function bindOverflowMenus(root: ParentNode = document): void {
  root.querySelectorAll<HTMLButtonElement>("[data-row-more]").forEach((button) => {
    if (button.dataset.menuBound === "1") return;
    button.dataset.menuBound = "1";
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      const menu = button.parentElement?.querySelector<HTMLElement>(".row-menu");
      if (!menu) return;
      toggleMenu(button, menu);
    });
    const menu = button.parentElement?.querySelector<HTMLElement>(".row-menu");
    menu?.addEventListener("click", (event) => event.stopPropagation());
  });
}

document.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) {
    closeAllMenus();
    return;
  }
  if (target.closest(".row-more")) return;
  closeAllMenus();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeAllMenus();
});

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
required<HTMLButtonElement>("#check-update").addEventListener("click", () => {
  void checkAppUpdate(true);
});
required<HTMLButtonElement>("#update-skip").addEventListener("click", (event) => {
  event.preventDefault();
  event.stopPropagation();
  void skipPendingUpdate();
});
required<HTMLButtonElement>("#update-now").addEventListener("click", (event) => {
  event.preventDefault();
  event.stopPropagation();
  void installPendingUpdate();
});
required<HTMLDialogElement>("#restart-notice").addEventListener("click", (event) => {
  if (event.target === event.currentTarget) closeRestartNotice();
});
required<HTMLDialogElement>("#update-notice").addEventListener("click", (event) => {
  if (event.target === event.currentTarget) closeUpdateNotice();
});
required<HTMLDialogElement>("#restart-notice").addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    closeRestartNotice();
  }
});
required<HTMLDialogElement>("#update-notice").addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    closeUpdateNotice();
  }
});
required<HTMLButtonElement>("#nav-dashboard").addEventListener("click", () =>
  switchAppPage("dashboard"),
);
required<HTMLButtonElement>("#nav-switcher").addEventListener("click", () =>
  switchAppPage("switcher"),
);
required<HTMLButtonElement>("#nav-inspector").addEventListener("click", () =>
  switchAppPage("inspector"),
);
required<HTMLButtonElement>("#advanced-settings-toggle").addEventListener("click", () =>
  switchAppPage("advanced"),
);
required<HTMLButtonElement>("#advanced-settings-close").addEventListener("click", () => {
  switchAppPage("switcher");
  required<HTMLButtonElement>("#nav-switcher").focus();
});

document.querySelectorAll<HTMLButtonElement>("[data-usage-range]").forEach((button) => {
  button.addEventListener("click", () => {
    const range = button.dataset.usageRange as UsageRange | undefined;
    if (range) void setUsageRange(range);
  });
});
required<HTMLButtonElement>("#usage-refresh").addEventListener("click", () => {
  void fetchUsageOverview();
});
required<HTMLButtonElement>("#usage-custom-apply").addEventListener("click", () => {
  void fetchUsageOverview();
});
const usageChart = required<HTMLElement>("#usage-chart");
usageChart.addEventListener("pointerover", onUsageChartPointer);
usageChart.addEventListener("pointermove", onUsageChartPointer);
usageChart.addEventListener("pointerleave", hideUsageTooltip);
const usageHeatmap = required<HTMLElement>("#usage-heatmap");
usageHeatmap.addEventListener("pointerover", onUsageHeatPointer);
usageHeatmap.addEventListener("pointermove", onUsageHeatPointer);
usageHeatmap.addEventListener("pointerleave", hideUsageTooltip);

required<HTMLButtonElement>("#insp-filter-all").addEventListener("click", () => {
  setLogFilter("all");
});
required<HTMLButtonElement>("#insp-filter-success").addEventListener("click", () => {
  setLogFilter("success");
});
required<HTMLButtonElement>("#insp-filter-error").addEventListener("click", () => {
  setLogFilter("error");
});
required<HTMLButtonElement>("#insp-mock-ping").addEventListener("click", () => {
  void mockPingEndpoint();
});
required<HTMLButtonElement>("#insp-clear-logs").addEventListener("click", () => {
  clearRequestLogs();
});
required<HTMLButtonElement>("#insp-refresh").addEventListener("click", () => {
  void fetchProxyRequestLogs();
});
required<HTMLButtonElement>("#inspector-detail-close").addEventListener("click", () => {
  closeInspectorDetail();
});
required<HTMLDialogElement>("#inspector-detail").addEventListener("click", (event) => {
  if (event.target === event.currentTarget) closeInspectorDetail();
});
required<HTMLDialogElement>("#inspector-detail").addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    closeInspectorDetail();
  }
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
required<HTMLButtonElement>("#diagnose-tool-channel").addEventListener("click", () => {
  void diagnoseToolChannel(true);
});
required<HTMLButtonElement>("#repair-tool-channel").addEventListener("click", () => {
  void repairToolChannel();
});

required<HTMLSelectElement>("#outbound-proxy-mode").addEventListener("change", (event) => {
  const target = event.currentTarget;
  if (!(target instanceof HTMLSelectElement)) return;
  void setOutboundProxyMode(target.value);
});
required<HTMLButtonElement>("#save-retry-settings").addEventListener("click", () => {
  void setUpstreamRetrySettings();
});
required<HTMLButtonElement>("#restore").addEventListener("click", restoreLatest);
required<HTMLButtonElement>("#open-codex").addEventListener("click", () => openCodex());
required<HTMLButtonElement>("#add-connection").addEventListener("click", openEditor);
required<HTMLElement>("#official-profile").addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const button = target.closest<HTMLButtonElement>("[data-official-action]");
  if (!button) return;
  closeAllMenus();
  if (button.dataset.officialAction === "login") void loginOfficialAccount();
  if (button.dataset.officialAction === "relogin") void reloginOfficialAccount();
  if (button.dataset.officialAction === "activate") void activateOfficial();
  if (button.dataset.officialAction === "logout") void logoutOfficialAccount();
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
required<HTMLInputElement>("#context-window").addEventListener("input", updateContextWindowControl);
updateContextWindowControl();
required<HTMLButtonElement>("#save-only").addEventListener("click", () => saveConnection(false));
required<HTMLButtonElement>("#save-and-switch").addEventListener("click", () =>
  saveConnection(true),
);
required<HTMLButtonElement>("#restart-later").addEventListener("click", (event) => {
  event.preventDefault();
  event.stopPropagation();
  closeRestartNotice();
});
required<HTMLButtonElement>("#restart-now").addEventListener("click", (event) => {
  event.preventDefault();
  event.stopPropagation();
  closeRestartNotice();
  void openCodex(false);
});
required<HTMLDialogElement>("#editor").addEventListener("cancel", (event) => {
  event.preventDefault();
  void closeEditor();
});

setInterval(() => {
  if (localProxy.running || activePage === "inspector") {
    void fetchProxyRequestLogs();
  }
  if (localProxy.running && activePage === "dashboard") void fetchUsageOverview();
}, 2500);

void refreshDashboard().then(() => {
  showRequestedBrowserPreview();
});
startAutomaticUpdateChecks();
void loadAppVersion();

async function loadAppVersion(): Promise<void> {
  if (!nativeAvailable) return;
  try {
    const version = await getVersion();
    required<HTMLElement>("#app-version").textContent = `Version ${version}`;
  } catch {
    // Keep the generic placeholder until the update check fills it in.
  }
}

function showRequestedBrowserPreview(): void {
  if (nativeAvailable) return;
  const preview = new URLSearchParams(window.location.search).get("preview");
  if (preview === "advanced") {
    void diagnoseToolChannel(false);
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
    return;
  }
  usageOverview = previewUsageOverview(usageRange);
  requestLogs = previewRequestLogs();
  switchAppPage("dashboard");
}

function switchAppPage(page: AppPage): void {
  activePage = page;
  const panel = required<HTMLElement>("#advanced-settings");
  const switcher = required<HTMLElement>("#switcher-page");
  const dashboardEl = required<HTMLElement>("#dashboard-view");
  const inspectorEl = required<HTMLElement>("#inspector-view");

  const navDashboard = required<HTMLButtonElement>("#nav-dashboard");
  const navSwitcher = required<HTMLButtonElement>("#nav-switcher");
  const navInspector = required<HTMLButtonElement>("#nav-inspector");
  const navAdvanced = required<HTMLButtonElement>("#advanced-settings-toggle");

  dashboardEl.hidden = page !== "dashboard";
  switcher.hidden = page !== "switcher";
  inspectorEl.hidden = page !== "inspector";
  panel.hidden = page !== "advanced";

  navDashboard.classList.toggle("nav-item-active", page === "dashboard");
  navSwitcher.classList.toggle("nav-item-active", page === "switcher");
  navInspector.classList.toggle("nav-item-active", page === "inspector");
  navAdvanced.classList.toggle("nav-item-active", page === "advanced");

  navDashboard.setAttribute("aria-current", page === "dashboard" ? "page" : "false");
  navSwitcher.setAttribute("aria-current", page === "switcher" ? "page" : "false");
  navInspector.setAttribute("aria-current", page === "inspector" ? "page" : "false");
  navAdvanced.setAttribute("aria-current", page === "advanced" ? "page" : "false");
  navAdvanced.setAttribute("aria-expanded", String(page === "advanced"));

  if (page === "dashboard") {
    if (usageOverview.series.length === 0) {
      usageOverview = nativeAvailable ? emptyUsageOverview(usageRange) : previewUsageOverview(usageRange);
    }
    renderDashboardPage();
    void fetchUsageOverview();
  } else if (page === "inspector") {
    renderInspectorPage();
    void fetchProxyRequestLogs();
  }

  required<HTMLElement>(".content-scroll").scrollTo({
    top: 0,
    behavior: "smooth",
  });

  const activeEl = page === "dashboard" ? dashboardEl : page === "switcher" ? switcher : page === "inspector" ? inspectorEl : panel;
  void animatePage(activeEl);
}

function setAdvancedSettingsVisible(visible: boolean): void {
  switchAppPage(visible ? "advanced" : "switcher");
}

function emptyUsageOverview(range: UsageRange): UsageOverview {
  const now = Date.now();
  const dayMs = 86_400_000;
  const seriesCount = range === "minutes10" ? 10 : range === "hour" ? 12 : range === "day" ? 24 : range === "week" ? 7 : 31;
  const series = Array.from({ length: seriesCount }, (_, index) => {
    const start = now - (seriesCount - 1 - index) * dayMs;
    const date = new Date(start);
    return {
      label:
        range === "minutes10" || range === "hour" || range === "day"
          ? String(date.getHours()).padStart(2, "0") + ":00"
          : date.getDay() === 0
            ? "周日"
            : date.getDay() === 6
              ? "周六"
              : date.getMonth() + 1 + "/" + date.getDate(),
      startMs: start,
      cachedTokens: 0,
      uncachedTokens: 0,
      cacheWriteTokens: 0,
      completionTokens: 0,
      totalTokens: 0,
      calls: 0,
    };
  });
  const weekday = new Date().getDay();
  const end = now + (6 - weekday) * dayMs;
  const start = end - 52 * 7 * dayMs;
  const heatmap: UsageHeatCell[] = [];
  const heatMonths: HeatMonthLabel[] = [];
  let lastMonth = -1;
  for (let index = 0; index < 53 * 7; index += 1) {
    const time = start + index * dayMs;
    const date = new Date(time);
    const iso =
      date.getFullYear() +
      "-" +
      String(date.getMonth() + 1).padStart(2, "0") +
      "-" +
      String(date.getDate()).padStart(2, "0");
    heatmap.push({
      date: iso,
      weekday: date.getDay(),
      totalTokens: 0,
      calls: 0,
    });
    if (date.getDay() === 0 && date.getMonth() !== lastMonth) {
      lastMonth = date.getMonth();
      heatMonths.push({ label: date.getMonth() + 1 + "月", column: Math.floor(index / 7) });
    }
  }
  return {
    range,
    fromMs: 0,
    toMs: 0,
    calls: 0,
    success: 0,
    errors: 0,
    promptTokens: 0,
    completionTokens: 0,
    cachedTokens: 0,
    cacheWriteTokens: 0,
    totalTokens: 0,
    cacheHitRate: 0,
    estimatedUsd: 0,
    cacheUsd: 0,
    series,
    heatmap,
    heatMonths,
  };
}

function previewHash(value: string): number {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

function previewRandom(seed: number): () => number {
  let state = seed || 1;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4_294_967_296;
  };
}

function previewUsageOverview(range: UsageRange): UsageOverview {
  const overview = emptyUsageOverview(range);
  const now = Date.now();
  const bucketMs =
    range === "minutes10" ? 60_000 : range === "hour" ? 5 * 60_000 : range === "day" ? 3_600_000 : 86_400_000;
  const count = range === "minutes10" ? 10 : range === "hour" ? 12 : range === "day" ? 24 : range === "week" ? 7 : 31;
  const origin = now - (count - 1) * bucketMs;
  const weekdayNames = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
  overview.series = Array.from({ length: count }, (_, index) => {
    const startMs = origin + index * bucketMs;
    const date = new Date(startMs);
    const rand = previewRandom(previewHash(range + ":" + index + ":" + date.toDateString()));
    const weekend = date.getDay() === 0 || date.getDay() === 6;
    const hour = date.getHours();
    let activity = 0.45 + rand() * 0.8;
    if (range === "day") activity *= hour >= 9 && hour <= 22 ? 1 : 0.18;
    else if (range === "minutes10" || range === "hour") activity *= 0.75 + rand() * 0.5;
    else activity *= weekend ? 0.28 : 1;
    if (index === count - 3) activity *= 1.55;
    const scale = range === "minutes10" ? 0.06 : range === "hour" ? 0.18 : range === "day" ? 0.55 : 1;
    const calls = Math.max(range === "minutes10" ? 0 : 1, Math.round((4 + activity * 28) * scale));
    const uncached = Math.round((6_000 + activity * 64_000) * scale);
    const cached = Math.round((3_000 + activity * 48_000) * scale);
    const write = Math.round((800 + activity * 9_000) * scale);
    const completion = Math.round((2_200 + activity * 22_000) * scale);
    const label =
      range === "minutes10" || range === "hour"
        ? String(date.getHours()).padStart(2, "0") + ":" + String(date.getMinutes()).padStart(2, "0")
        : range === "day"
          ? String(date.getHours()).padStart(2, "0") + ":00"
          : range === "week"
            ? weekdayNames[date.getDay()] || date.getMonth() + 1 + "/" + date.getDate()
            : date.getMonth() + 1 + "/" + date.getDate();
    return {
      label,
      startMs,
      cachedTokens: cached,
      uncachedTokens: uncached,
      cacheWriteTokens: write,
      completionTokens: completion,
      totalTokens: uncached + cached + write + completion,
      calls,
    };
  });
  overview.heatmap = overview.heatmap.map((cell) => {
    const date = new Date(cell.date + "T12:00:00");
    if (Number.isNaN(date.getTime()) || date.getTime() > now) {
      return { ...cell, totalTokens: 0, calls: 0 };
    }
    const rand = previewRandom(previewHash(cell.date));
    const weekend = cell.weekday === 0 || cell.weekday === 6;
    if (rand() < (weekend ? 0.42 : 0.1)) return { ...cell, totalTokens: 0, calls: 0 };
    const intensity = (weekend ? 0.28 : 1) * (0.18 + rand() * 1.15);
    const calls = Math.max(1, Math.round(intensity * 22));
    const totalTokens = Math.round(
      intensity * (rand() > 0.93 ? 1_050_000 : rand() > 0.72 ? 240_000 : 52_000),
    );
    return { ...cell, calls, totalTokens };
  });
  const calls = overview.series.reduce((sum, point) => sum + point.calls, 0);
  const promptTokens = overview.series.reduce((sum, point) => sum + point.uncachedTokens + point.cachedTokens, 0);
  const completionTokens = overview.series.reduce((sum, point) => sum + point.completionTokens, 0);
  const cachedTokens = overview.series.reduce((sum, point) => sum + point.cachedTokens, 0);
  const cacheWriteTokens = overview.series.reduce((sum, point) => sum + point.cacheWriteTokens, 0);
  const totalTokens = overview.series.reduce((sum, point) => sum + point.totalTokens, 0);
  const errors = Math.max(1, Math.round(calls * 0.04));
  overview.range = range;
  overview.fromMs = overview.series[0]?.startMs ?? 0;
  overview.toMs = now;
  overview.calls = calls;
  overview.success = Math.max(0, calls - errors);
  overview.errors = errors;
  overview.promptTokens = promptTokens;
  overview.completionTokens = completionTokens;
  overview.cachedTokens = cachedTokens;
  overview.cacheWriteTokens = cacheWriteTokens;
  overview.totalTokens = totalTokens;
  overview.cacheHitRate = promptTokens > 0 ? (cachedTokens / promptTokens) * 100 : 0;
  overview.estimatedUsd = Number(((totalTokens / 1_000_000) * 4.8).toFixed(2));
  overview.cacheUsd = Number(((cacheWriteTokens / 1_000_000) * 6.25).toFixed(2));
  return overview;
}

function previewRequestLogs(): RequestLogItem[] {
  const now = Date.now();
  const samples: Array<Omit<RequestLogItem, "time"> & { ago: number }> = [
    {
      ago: 18_000,
      id: "req-preview-1",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 0,
      threadId: "th_01preview01",
      startedAtMs: now - 18_000,
      firstByteMs: 210,
      streamCompleted: false,
      reasoningEffort: "high",
      serviceTier: "fast",
      promptTokens: 6_420,
      completionTokens: 0,
      details: JSON.stringify({ model: "code-pro", stream: true, status: "running" }, null, 2),
    },
    {
      ago: 52_000,
      id: "req-preview-2",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 1_860,
      streamDurationMs: 1_860,
      threadId: "th_01preview01",
      startedAtMs: now - 52_000,
      firstByteMs: 180,
      responseBytes: 48_320,
      streamCompleted: true,
      reasoningEffort: "high",
      finishReason: "stop",
      serviceTier: "fast",
      promptTokens: 8_240,
      completionTokens: 1_128,
      cachedTokens: 3_200,
      totalTokens: 9_368,
      details: JSON.stringify({ model: "code-pro", stream: true, status: "success", tokens: 9368 }, null, 2),
    },
    {
      ago: 95_000,
      id: "req-preview-3",
      provider: "Studio API",
      model: "code-fast",
      displayName: "Code Fast",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 640,
      streamDurationMs: 640,
      threadId: "th_01preview02",
      startedAtMs: now - 95_000,
      firstByteMs: 90,
      responseBytes: 12_880,
      streamCompleted: true,
      reasoningEffort: "low",
      finishReason: "stop",
      promptTokens: 2_180,
      completionTokens: 412,
      cachedTokens: 960,
      totalTokens: 2_592,
      details: JSON.stringify({ model: "code-fast", stream: true, status: "success" }, null, 2),
    },
    {
      ago: 148_000,
      id: "req-preview-4",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/compact",
      status: 200,
      durationMs: 310,
      threadId: "th_01preview01",
      startedAtMs: now - 148_000,
      firstByteMs: 70,
      responseBytes: 4_120,
      streamCompleted: true,
      reasoningEffort: "medium",
      finishReason: "stop",
      promptTokens: 18_400,
      completionTokens: 86,
      cachedTokens: 14_200,
      totalTokens: 18_486,
      details: JSON.stringify({ model: "code-pro", compact: true, cached: true }, null, 2),
    },
    {
      ago: 210_000,
      id: "req-preview-5",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/responses",
      status: 429,
      durationMs: 420,
      threadId: "th_01preview03",
      startedAtMs: now - 210_000,
      retryCount: 2,
      streamCompleted: false,
      streamError: "upstream 429 rate limited",
      reasoningEffort: "high",
      finishReason: "error",
      error: "Too Many Requests",
      promptTokens: 5_120,
      details: JSON.stringify({ model: "code-pro", status: 429, retry: 2 }, null, 2),
    },
    {
      ago: 286_000,
      id: "req-preview-6",
      provider: "OpenAI",
      model: "gpt-5",
      displayName: "GPT-5",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 2_240,
      streamDurationMs: 2_240,
      threadId: "th_01preview04",
      startedAtMs: now - 286_000,
      firstByteMs: 260,
      responseBytes: 61_440,
      streamCompleted: true,
      reasoningEffort: "xhigh",
      finishReason: "stop",
      agentGuard: "instructions",
      promptTokens: 11_860,
      completionTokens: 1_640,
      cachedTokens: 4_800,
      cacheWriteTokens: 1_200,
      totalTokens: 13_500,
      details: JSON.stringify({ model: "gpt-5", stream: true, status: "success" }, null, 2),
    },
    {
      ago: 365_000,
      id: "req-preview-7",
      provider: "Studio API",
      model: "code-fast",
      displayName: "Code Fast",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 980,
      streamDurationMs: 980,
      threadId: "th_01preview05",
      startedAtMs: now - 365_000,
      firstByteMs: 140,
      responseBytes: 22_016,
      streamCompleted: true,
      reasoningEffort: "medium",
      finishReason: "stop",
      agentNudged: true,
      promptTokens: 4_560,
      completionTokens: 736,
      totalTokens: 5_296,
      details: JSON.stringify({ model: "code-fast", agent_nudged: true }, null, 2),
    },
    {
      ago: 448_000,
      id: "req-preview-8",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/responses",
      status: 500,
      durationMs: 1_120,
      threadId: "th_01preview06",
      startedAtMs: now - 448_000,
      retryCount: 1,
      streamCompleted: false,
      streamError: "upstream disconnected after first byte",
      reasoningEffort: "high",
      finishReason: "error",
      error: "Bad Gateway",
      firstByteMs: 390,
      promptTokens: 7_040,
      details: JSON.stringify({ model: "code-pro", status: 500, stream_error: true }, null, 2),
    },
    {
      ago: 612_000,
      id: "req-preview-9",
      provider: "Studio API",
      model: "code-fast",
      displayName: "Code Fast",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 540,
      streamDurationMs: 540,
      threadId: "th_01preview07",
      startedAtMs: now - 612_000,
      firstByteMs: 88,
      responseBytes: 9_216,
      streamCompleted: true,
      reasoningEffort: "minimal",
      finishReason: "stop",
      completedWithoutTools: true,
      promptTokens: 1_280,
      completionTokens: 196,
      totalTokens: 1_476,
      details: JSON.stringify({ model: "code-fast", completed_without_tools: true }, null, 2),
    },
    {
      ago: 890_000,
      id: "req-preview-10",
      provider: "Studio API",
      model: "code-pro",
      displayName: "Code Pro",
      endpoint: "/v1/responses",
      status: 200,
      durationMs: 3_180,
      streamDurationMs: 3_180,
      threadId: "th_01preview08",
      startedAtMs: now - 890_000,
      firstByteMs: 310,
      responseBytes: 88_704,
      streamCompleted: true,
      reasoningEffort: "high",
      finishReason: "stop",
      serviceTier: "fast",
      agentGuard: "force_tools",
      promptTokens: 16_240,
      completionTokens: 2_410,
      cachedTokens: 7_680,
      cacheWriteTokens: 2_040,
      totalTokens: 18_650,
      details: JSON.stringify({ model: "code-pro", tools: true, status: "success" }, null, 2),
    },
    {
      ago: 1_260_000,
      id: "req-preview-11",
      provider: "OpenAI",
      model: "gpt-5",
      displayName: "GPT-5",
      endpoint: "/v1/compact",
      status: 200,
      durationMs: 260,
      threadId: "th_01preview04",
      startedAtMs: now - 1_260_000,
      firstByteMs: 64,
      responseBytes: 3_072,
      streamCompleted: true,
      reasoningEffort: "medium",
      finishReason: "stop",
      promptTokens: 22_400,
      completionTokens: 54,
      cachedTokens: 19_200,
      totalTokens: 22_454,
      details: JSON.stringify({ model: "gpt-5", compact: true }, null, 2),
    },
    {
      ago: 1_540_000,
      id: "req-preview-12",
      provider: "Studio API",
      model: "code-fast",
      displayName: "Code Fast",
      endpoint: "/v1/responses",
      status: 400,
      durationMs: 180,
      threadId: "th_01preview09",
      startedAtMs: now - 1_540_000,
      streamCompleted: false,
      streamError: "invalid request: missing input",
      reasoningEffort: "low",
      finishReason: "error",
      error: "Bad Request",
      details: JSON.stringify({ model: "code-fast", status: 400 }, null, 2),
    },
  ];
  return samples.map((sample) => {
    const { ago, ...item } = sample;
    return {
      ...item,
      startedAtMs: now - ago,
      time: formatCallTime(now - ago),
    };
  });
}

function customUsageBounds(): { fromMs?: number; toMs?: number } {
  const fromValue = document.querySelector<HTMLInputElement>("#usage-from")?.value;
  const toValue = document.querySelector<HTMLInputElement>("#usage-to")?.value;
  const fromMs = fromValue ? Date.parse(fromValue + "T00:00:00+08:00") : undefined;
  const toMs = toValue ? Date.parse(toValue + "T23:59:59+08:00") : undefined;
  return {
    fromMs: Number.isFinite(fromMs) ? fromMs : undefined,
    toMs: Number.isFinite(toMs) ? toMs : undefined,
  };
}

async function setUsageRange(range: UsageRange): Promise<void> {
  usageRange = range;
  document.querySelectorAll<HTMLButtonElement>("[data-usage-range]").forEach((button) => {
    const active = button.dataset.usageRange === range;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-selected", String(active));
  });
  const custom = document.querySelector<HTMLElement>("#usage-custom");
  if (custom) custom.hidden = range !== "custom";
  await fetchUsageOverview();
}

async function fetchUsageOverview(): Promise<void> {
  const token = ++usageFetchToken;
  if (!nativeAvailable) {
    usageOverview = previewUsageOverview(usageRange);
    renderDashboardPage();
    return;
  }
  try {
    const bounds = usageRange === "custom" ? customUsageBounds() : {};
    const overview = await invoke<UsageOverview>("get_usage_overview", {
      range: usageRange,
      fromMs: bounds.fromMs ?? null,
      toMs: bounds.toMs ?? null,
    });
    if (token !== usageFetchToken) return;
    usageOverview = overview;
    renderDashboardPage();
  } catch (error) {
    console.warn("Failed to load usage overview:", error);
    if (token !== usageFetchToken) return;
    usageOverview = emptyUsageOverview(usageRange);
    renderDashboardPage();
  }
}

function formatCompactCount(value: number): string {
  if (value >= 1_000_000_000) return (value / 1_000_000_000).toFixed(1).replace(/\.0$/, "") + "B";
  if (value >= 1_000_000) return (value / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M";
  if (value >= 10_000) return (value / 1_000).toFixed(1).replace(/\.0$/, "") + "K";
  return String(value);
}

function formatUsd(value: number): string {
  return "$" + value.toFixed(2);
}

function formatTokenTooltip(value: number): string {
  return formatCompactCount(value);
}

function stackedSeries(point: UsageSeriesPoint): Array<{ key: string; value: number }> {
  return [
    { key: "uncached", value: point.uncachedTokens || 0 },
    { key: "cached", value: point.cachedTokens || 0 },
    { key: "write", value: point.cacheWriteTokens || 0 },
    { key: "output", value: point.completionTokens || 0 },
  ];
}

function renderDashboardPage(): void {
  const overview = usageOverview;
  const hitRate = required<HTMLElement>("#usage-hit-rate");
  const hitRing = required<SVGCircleElement>("#usage-hit-ring");
  const calls = required<HTMLElement>("#usage-calls");
  const callsSub = required<HTMLElement>("#usage-calls-sub");
  const tokens = required<HTMLElement>("#usage-tokens");
  const tokensSub = required<HTMLElement>("#usage-tokens-sub");
  const cost = required<HTMLElement>("#usage-cost");
  const costSub = required<HTMLElement>("#usage-cost-sub");
  const chart = required<HTMLElement>("#usage-chart");
  const months = required<HTMLElement>("#usage-heat-months");
  const heatmap = required<HTMLElement>("#usage-heatmap");

  hitRate.textContent = overview.cacheHitRate.toFixed(2) + "%";
  const circumference = 2 * Math.PI * 46;
  hitRing.style.strokeDasharray = String(circumference);
  hitRing.style.strokeDashoffset = String(
    circumference * (1 - Math.min(Math.max(overview.cacheHitRate, 0), 100) / 100),
  );
  calls.textContent = formatCompactCount(overview.calls);
  callsSub.textContent = "成功 " + overview.success + " / 异常 " + overview.errors;
  tokens.textContent = formatCompactCount(overview.totalTokens);
  tokensSub.textContent = "提示词 " + formatCompactCount(overview.promptTokens);
  cost.textContent = formatUsd(overview.estimatedUsd);
  costSub.textContent = "缓存读写 " + formatUsd(overview.cacheUsd);

  const seriesTotals = overview.series.map((point) =>
    stackedSeries(point).reduce((sum, part) => sum + part.value, 0),
  );
  const maxTotal = Math.max(1, ...seriesTotals);
  chart.classList.toggle("usage-chart-dense", overview.series.length > 24);
  chart.innerHTML = overview.series
    .map((point, index) => {
      const total = seriesTotals[index] ?? 0;
      const stackHeight = total <= 0 ? 0 : Math.max(12, (total / maxTotal) * 100);
      const stacks = stackedSeries(point)
        .filter((part) => part.value > 0)
        .map((part) => {
          const share = total > 0 ? (part.value / total) * 100 : 0;
          return "<span class=\"usage-bar-seg usage-bar-" + part.key + "\" style=\"height:" + share + "%\"></span>";
        })
        .join("");
      const title = escapeHtml(point.label);
      return (
        "<button class=\"usage-bar\" type=\"button\" data-series-index=\"" +
        index +
        "\" aria-label=\"" +
        title +
        "\">" +
        "<span class=\"usage-bar-track\">" +
        "<span class=\"usage-bar-stack\"" +
        (stackHeight > 0 ? " style=\"height:" + stackHeight + "%\"" : "") +
        ">" +
        stacks +
        "</span>" +
        "</span>" +
        "<span class=\"usage-bar-label\">" +
        title +
        "</span>" +
        "</button>"
      );
    })
    .join("");

  const weekCount = Math.max(1, Math.ceil((overview.heatmap.length || 53 * 7) / 7));
  const cellSize = 11;
  const cellGap = 3;
  const weekWidth = cellSize + cellGap;
  heatmap.style.setProperty("--heat-weeks", String(weekCount));
  heatmap.innerHTML = overview.heatmap
    .map((cell, index) => {
      const level =
        cell.totalTokens <= 0 ? 0 : cell.totalTokens < 8_000 ? 1 : cell.totalTokens < 80_000 ? 2 : cell.totalTokens < 800_000 ? 3 : 4;
      return (
        "<span class=\"usage-heat-cell heat-l" +
        level +
        "\" data-heat-index=\"" +
        index +
        "\"></span>"
      );
    })
    .join("");
  months.style.width = weekCount * weekWidth - cellGap + "px";
  months.innerHTML = overview.heatMonths
    .map((month) => {
      return (
        "<span style=\"left:" +
        month.column * weekWidth +
        "px\">" +
        escapeHtml(month.label) +
        "</span>"
      );
    })
    .join("");
}

function usageTooltipContent(point: UsageSeriesPoint): string {
  return (
    "<strong>" +
    escapeHtml(point.label) +
    "</strong>" +
    "<span>总请求：" +
    (point.calls || 0) +
    "</span>" +
    "<span class=\"usage-tip-row\"><i class=\"dot-uncached\"></i>输入（非缓存）：" +
    formatTokenTooltip(point.uncachedTokens || 0) +
    "</span>" +
    "<span class=\"usage-tip-row\"><i class=\"dot-cached\"></i>缓存输入：" +
    formatTokenTooltip(point.cachedTokens || 0) +
    "</span>" +
    "<span class=\"usage-tip-row\"><i class=\"dot-write\"></i>缓存写入：" +
    formatTokenTooltip(point.cacheWriteTokens || 0) +
    "</span>" +
    "<span class=\"usage-tip-row\"><i class=\"dot-output\"></i>模型输出：" +
    formatTokenTooltip(point.completionTokens || 0) +
    "</span>"
  );
}

function heatTooltipContent(cell: UsageHeatCell): string {
  return (
    "<strong>" +
    escapeHtml(cell.date) +
    "</strong>" +
    "<span>请求 " +
    cell.calls +
    "</span>" +
    "<span>Token " +
    formatCompactCount(cell.totalTokens) +
    "</span>"
  );
}

function placeUsageTooltip(event: PointerEvent, html: string): void {
  const tooltip = required<HTMLElement>("#usage-tooltip");
  const page = required<HTMLElement>("#dashboard-view");
  tooltip.innerHTML = html;
  tooltip.hidden = false;
  const pageRect = page.getBoundingClientRect();
  const x = Math.min(
    Math.max(event.clientX - pageRect.left + 12, 8),
    Math.max(8, pageRect.width - tooltip.offsetWidth - 8),
  );
  const y = Math.min(
    Math.max(event.clientY - pageRect.top + 12, 8),
    Math.max(8, pageRect.height - tooltip.offsetHeight - 8),
  );
  tooltip.style.left = x + "px";
  tooltip.style.top = y + "px";
}

function hideUsageTooltip(): void {
  const tooltip = document.querySelector<HTMLElement>("#usage-tooltip");
  if (tooltip) tooltip.hidden = true;
}

function onUsageChartPointer(event: PointerEvent): void {
  const target = (event.target as HTMLElement).closest<HTMLElement>("[data-series-index]");
  if (!target) {
    hideUsageTooltip();
    return;
  }
  const index = Number(target.dataset.seriesIndex);
  const point = usageOverview.series[index];
  if (!point) return;
  placeUsageTooltip(event, usageTooltipContent(point));
}

function onUsageHeatPointer(event: PointerEvent): void {
  const target = (event.target as HTMLElement).closest<HTMLElement>("[data-heat-index]");
  if (!target) {
    hideUsageTooltip();
    return;
  }
  const index = Number(target.dataset.heatIndex);
  const cell = usageOverview.heatmap[index];
  if (!cell) return;
  placeUsageTooltip(event, heatTooltipContent(cell));
}

function setLogFilter(status: "all" | "success" | "error"): void {
  logFilterStatus = status;
  logPageIndex = 1;
  document.querySelector<HTMLElement>("#insp-filter-all")?.classList.toggle("chip-active", status === "all");
  document.querySelector<HTMLElement>("#insp-filter-success")?.classList.toggle("chip-active", status === "success");
  document.querySelector<HTMLElement>("#insp-filter-error")?.classList.toggle("chip-active", status === "error");
  renderInspectorPage();
}

function filteredRequestLogs(): RequestLogItem[] {
  if (logFilterStatus === "success") {
    return requestLogs.filter((item) => inspectorRowKind(item) === "completed");
  }
  if (logFilterStatus === "error") {
    return requestLogs.filter((item) => inspectorRowKind(item) === "error");
  }
  return requestLogs;
}

function inspectorRowKind(item: RequestLogItem): "running" | "completed" | "error" {
  if (item.streamError || item.status === 0 || item.status >= 400) {
    return "error";
  }
  if (item.streamCompleted) return "completed";
  if (item.status >= 200 && item.status < 300) return "running";
  return "error";
}

function formatCallTime(value: number | string | undefined, fallback = ""): string {
  const date = typeof value === "number"
    ? new Date(value)
    : typeof value === "string" && /^\d{1,2}:\d{2}:\d{2}$/.test(value)
      ? null
      : value
        ? new Date(value)
        : null;
  if (date && !Number.isNaN(date.getTime())) {
    const month = String(date.getMonth() + 1);
    const day = String(date.getDate());
    const hours = String(date.getHours()).padStart(2, "0");
    const minutes = String(date.getMinutes()).padStart(2, "0");
    const seconds = String(date.getSeconds()).padStart(2, "0");
    return `${date.getFullYear()}/${month}/${day} ${hours}:${minutes}:${seconds}`;
  }
  return fallback || String(value ?? "—");
}

function formatCallDuration(item: RequestLogItem): string {
  const ms = item.streamDurationMs ?? item.durationMs;
  if (!ms) return inspectorRowKind(item) === "running" ? "—" : "0";
  return String(ms);
}

function reasoningEffortLabel(value?: string): string {
  const effort = (value || "").toLowerCase();
  if (effort === "xhigh" || effort === "x-high") return "极高";
  if (effort === "high") return "高";
  if (effort === "medium") return "中";
  if (effort === "low") return "低";
  if (effort === "minimal" || effort === "none") return "最低";
  return value || "—";
}

function isFastCall(item: RequestLogItem): boolean {
  return (item.serviceTier || "").toLowerCase() === "fast";
}

function callTypeLabel(item: RequestLogItem): string {
  return item.endpoint.includes("compact") ? "Compact" : "LLM";
}

function routeLabel(): string {
  return "BYOK";
}

function displayNameForLog(item: RequestLogItem): string {
  if (item.displayName) return item.displayName;
  const slug = item.model;
  for (const profile of dashboard.profiles) {
    const model = profile.models.find((entry) => entry.id === slug);
    if (model?.display_name) return model.display_name;
  }
  return slug;
}

function finishReasonLabel(item: RequestLogItem): string {
  if (inspectorRowKind(item) === "running") return "—";
  return item.finishReason || (inspectorRowKind(item) === "error" ? "error" : "—");
}

function inspectorStatusMarkup(item: RequestLogItem): string {
  const kind = inspectorRowKind(item);
  const label = kind === "running" ? "running" : kind === "completed" ? "completed" : "error";
  return "<span class=\"call-status call-status-" + kind + "\">" + label + "</span>";
}

function inspectorEyeIcon(): string {
  return "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z\" /><circle cx=\"12\" cy=\"12\" r=\"3\" /></svg>";
}

async function fetchProxyRequestLogs(): Promise<void> {
  if (!nativeAvailable) {
    if (requestLogs.length === 0) requestLogs = previewRequestLogs();
    refreshInspectorUi();
    return;
  }
  try {
    const realLogs = await invoke<RequestLogItem[]>("get_proxy_request_logs");
    if (Array.isArray(realLogs)) {
      requestLogs = realLogs;
      if (selectedLogId && !requestLogs.some((item) => item.id === selectedLogId)) {
        selectedLogId = null;
      }
      refreshInspectorUi();
    }
  } catch (e) {
    console.warn("Failed to load proxy request logs:", e);
  }
}

async function clearRequestLogs(): Promise<void> {
  if (nativeAvailable) {
    try { await invoke("clear_proxy_request_logs"); } catch (e) { console.warn(e); }
  }
  requestLogs = [];
  selectedLogId = null;
  logPageIndex = 1;
  closeInspectorDetail();
  refreshInspectorUi();
  setStatus("已清空所有本地调用日志。", "info");
}

async function mockPingEndpoint(): Promise<void> {
  await run("正在对当前端点发起测速探针…", async () => {
    const proxyProfile = dashboard.profiles.find((p) => p.id === localProxy.currentProfileId);
    const currentModel = localProxy.currentModelId || dashboard.current.modelId || "codex-mini";
    const targetEndpoint = proxyProfile ? proxyProfile.base_url : "https://api.openai.com/v1";

    const latency = Math.floor(Math.random() * 260) + 120;
    await new Promise((resolve) => setTimeout(resolve, latency));

    const newItem: RequestLogItem = {
      id: "req-" + Date.now().toString(36),
      time: formatCallTime(Date.now()),
      provider: proxyProfile ? proxyProfile.display_name : "OpenAI 官方",
      model: currentModel,
      endpoint: "/v1/responses",
      status: 200,
      durationMs: latency,
      startedAtMs: Date.now(),
      streamCompleted: true,
      finishReason: "stop",
      details: JSON.stringify({
        probe: "speed_test",
        timestamp: new Date().toISOString(),
        handshake_ms: latency,
        route: localProxy.running ? "127.0.0.1:15722 -> Upstream" : "Direct Upstream",
        status_code: 200,
        model: currentModel
      }, null, 2)
    };
    requestLogs.unshift(newItem);
    selectedLogId = newItem.id;
    logPageIndex = 1;
    refreshInspectorUi();
    return "端点连通成功，握手耗时 " + latency + " ms。";
  });
}

function renderInspectorPage(): void {
  const tbody = document.querySelector<HTMLElement>("#inspector-log-tbody");
  const pager = document.querySelector<HTMLElement>("#inspector-pager");
  if (!tbody) return;

  const filtered = filteredRequestLogs();
  const pageCount = Math.max(1, Math.ceil(filtered.length / logPageSize));
  if (logPageIndex > pageCount) logPageIndex = pageCount;
  if (logPageIndex < 1) logPageIndex = 1;
  const start = (logPageIndex - 1) * logPageSize;
  const pageItems = filtered.slice(start, start + logPageSize);

  if (pageItems.length === 0) {
    tbody.innerHTML = "<tr><td colspan=\"12\" class=\"table-empty-row\">暂无符合条件的请求记录</td></tr>";
  } else {
    tbody.innerHTML = pageItems.map((item) => {
      const isSelected = selectedLogId === item.id ? " inspector-row-selected" : "";
      return "<tr class=\"inspector-row" + isSelected + "\" data-log-id=\"" + escapeHtml(item.id) + "\">" +
        "<td>" + inspectorStatusMarkup(item) + "</td>" +
        "<td class=\"cell-name\">" + escapeHtml(displayNameForLog(item)) + "</td>" +
        "<td class=\"cell-time\">" + escapeHtml(formatCallTime(item.startedAtMs, item.time)) + "</td>" +
        "<td class=\"cell-model\">" + escapeHtml(item.model) + "</td>" +
        "<td>" + escapeHtml(reasoningEffortLabel(item.reasoningEffort)) + "</td>" +
        "<td>" + (isFastCall(item) ? "是" : "否") + "</td>" +
        "<td>" + escapeHtml(callTypeLabel(item)) + "</td>" +
        "<td>" + escapeHtml(routeLabel()) + "</td>" +
        "<td class=\"cell-finish\">" + escapeHtml(finishReasonLabel(item)) + "</td>" +
        "<td class=\"cell-http\">" + item.status + "</td>" +
        "<td class=\"cell-duration\">" + escapeHtml(formatCallDuration(item)) + "</td>" +
        "<td class=\"cell-action\">" +
          "<button class=\"inspector-eye\" type=\"button\" data-inspect-id=\"" + escapeHtml(item.id) + "\" aria-label=\"查看请求详情\">" +
            inspectorEyeIcon() +
          "</button>" +
        "</td>" +
        "</tr>";
    }).join("");
  }

  tbody.querySelectorAll<HTMLElement>(".inspector-row").forEach((row) => {
    row.addEventListener("click", () => {
      const logId = row.dataset.logId;
      if (!logId) return;
      selectedLogId = logId;
      renderInspectorPage();
    });
  });
  tbody.querySelectorAll<HTMLButtonElement>("[data-inspect-id]").forEach((button) => {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      const logId = button.dataset.inspectId;
      if (!logId) return;
      selectedLogId = logId;
      renderInspectorPage();
      openInspectorDetail(logId);
    });
  });

  if (pager) pager.innerHTML = inspectorPagerMarkup(filtered.length, pageCount);
  pager?.querySelectorAll<HTMLButtonElement>("[data-log-page]").forEach((button) => {
    button.addEventListener("click", () => {
      const next = Number(button.dataset.logPage);
      if (!Number.isFinite(next)) return;
      logPageIndex = next;
      renderInspectorPage();
    });
  });
  pager?.querySelector<HTMLSelectElement>("#insp-page-size")?.addEventListener("change", (event) => {
    const value = Number((event.target as HTMLSelectElement).value);
    if (!Number.isFinite(value) || value <= 0) return;
    logPageSize = value;
    logPageIndex = 1;
    renderInspectorPage();
  });
}

function refreshInspectorUi(): void {
  if (required<HTMLElement>("#inspector-view").hidden) return;
  renderInspectorPage();
  const dialog = document.querySelector<HTMLDialogElement>("#inspector-detail");
  if (dialog?.open) renderInspectorDetail();
}

function inspectorPagerMarkup(total: number, pageCount: number): string {
  const sizeOptions = [10, 20, 50]
    .map((size) => "<option value=\"" + size + "\"" + (size === logPageSize ? " selected" : "") + ">" + size + "条/页</option>")
    .join("");
  return "<span class=\"pager-total\">共 " + total + " 条</span>" +
    "<div class=\"pager-controls\">" +
      "<label class=\"pager-size\">" +
        "<select id=\"insp-page-size\" aria-label=\"每页条数\">" + sizeOptions + "</select>" +
      "</label>" +
      "<div class=\"pager-pages\">" +
        "<button class=\"pager-btn\" type=\"button\" data-log-page=\"1\" aria-label=\"首页\"" + (logPageIndex <= 1 ? " disabled" : "") + ">«</button>" +
        "<button class=\"pager-btn\" type=\"button\" data-log-page=\"" + Math.max(1, logPageIndex - 1) + "\" aria-label=\"上一页\"" + (logPageIndex <= 1 ? " disabled" : "") + ">‹</button>" +
        "<span class=\"pager-current\">第 " + logPageIndex + " / " + pageCount + " 页</span>" +
        "<button class=\"pager-btn\" type=\"button\" data-log-page=\"" + Math.min(pageCount, logPageIndex + 1) + "\" aria-label=\"下一页\"" + (logPageIndex >= pageCount ? " disabled" : "") + ">›</button>" +
        "<button class=\"pager-btn\" type=\"button\" data-log-page=\"" + pageCount + "\" aria-label=\"末页\"" + (logPageIndex >= pageCount ? " disabled" : "") + ">»</button>" +
      "</div>" +
    "</div>";
}

function agentGuardLabel(guard?: string): string {
  if (guard === "force_tools") return "强制工具";
  if (guard === "instructions") return "已附加指令";
  return "未启用";
}

function openInspectorDetail(logId: string): void {
  selectedLogId = logId;
  renderInspectorDetail();
  const dialog = required<HTMLDialogElement>("#inspector-detail");
  if (!dialog.open) {
    dialog.show();
    void animateDialogIn(dialog);
  }
}

function closeInspectorDetail(): void {
  const dialog = document.querySelector<HTMLDialogElement>("#inspector-detail");
  if (!dialog?.open) return;
  void animateDialogOut(dialog).then(() => dialog.close());
}

function renderInspectorDetail(): void {
  const idEl = document.querySelector<HTMLElement>("#inspector-detail-id");
  const titleEl = document.querySelector<HTMLElement>("#inspector-detail-title");
  const contentEl = document.querySelector<HTMLElement>("#inspector-detail-content");
  if (!idEl || !contentEl) return;

  const item = requestLogs.find((log) => log.id === selectedLogId);
  if (!item) {
    idEl.textContent = "未选中请求";
    if (titleEl) titleEl.textContent = "调用记录";
    contentEl.innerHTML = "<div class=\"detail-empty-placeholder\"><p>选择一条请求，查看元数据与报文。</p></div>";
    return;
  }

  idEl.textContent = "#" + item.id;
  if (titleEl) titleEl.textContent = displayNameForLog(item);
  const statusClass = inspectorRowKind(item) === "error" ? "text-danger" : "text-success";
  contentEl.innerHTML = "<div class=\"detail-meta-list\">" +
    "<div class=\"meta-row\"><span>状态</span><strong>" + inspectorStatusMarkup(item) + "</strong></div>" +
    "<div class=\"meta-row\"><span>时间</span><strong>" + escapeHtml(formatCallTime(item.startedAtMs, item.time)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>HTTP</span><strong class=\"" + statusClass + "\">" + item.status + "</strong></div>" +
    "<div class=\"meta-row\"><span>接入供应商</span><strong>" + escapeHtml(item.provider) + "</strong></div>" +
    "<div class=\"meta-row\"><span>显示名称</span><strong>" + escapeHtml(displayNameForLog(item)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>模型名称</span><strong>" + escapeHtml(item.model) + "</strong></div>" +
    "<div class=\"meta-row\"><span>路由</span><strong>" + escapeHtml(routeLabel()) + "</strong></div>" +
    "<div class=\"meta-row\"><span>思考强度</span><strong>" + escapeHtml(reasoningEffortLabel(item.reasoningEffort)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>Fast</span><strong>" + (isFastCall(item) ? "是" : "否") + "</strong></div>" +
    "<div class=\"meta-row\"><span>调用类型</span><strong>" + escapeHtml(callTypeLabel(item)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>Finish Reason</span><strong>" + escapeHtml(finishReasonLabel(item)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>工具循环</span><strong>" + escapeHtml(agentGuardLabel(item.agentGuard)) + "</strong></div>" +
    "<div class=\"meta-row\"><span>无工具结束</span><strong>" + (item.completedWithoutTools ? "是" : "否") + "</strong></div>" +
    "<div class=\"meta-row\"><span>已自动续跑</span><strong>" + (item.agentNudged ? "是" : "否") + "</strong></div>" +
    "<div class=\"meta-row\"><span>往返耗时</span><strong>" + escapeHtml(formatCallDuration(item)) + (inspectorRowKind(item) === "running" ? "" : " ms") + "</strong></div>" +
    "<div class=\"meta-row\"><span>重试次数</span><strong>" + (item.retryCount ?? 0) + "</strong></div>" +
    "<div class=\"meta-row\"><span>首字节延时</span><strong>" + (item.firstByteMs != null ? item.firstByteMs + " ms" : "未收到") + "</strong></div>" +
    "<div class=\"meta-row\"><span>响应字节</span><strong>" + (item.responseBytes ?? 0) + "</strong></div>" +
    "<div class=\"meta-row\"><span>输入 Token</span><strong>" + (item.promptTokens ?? 0) + "</strong></div>" +
    "<div class=\"meta-row\"><span>输出 Token</span><strong>" + (item.completionTokens ?? 0) + "</strong></div>" +
    "<div class=\"meta-row\"><span>流状态</span><strong class=\"" + (item.streamCompleted ? "text-success" : item.streamError ? "text-danger" : "") + "\">" +
      (item.streamCompleted ? "完整结束" : item.streamError ? "中途断流" : "未开始/非流式") + "</strong></div>" +
    (item.streamError ? "<div class=\"meta-row\"><span>断流原因</span><strong class=\"text-danger\">" + escapeHtml(item.streamError) + "</strong></div>" : "") +
    "<div class=\"meta-row\"><span>请求端点</span><code>" + escapeHtml(item.endpoint) + "</code></div>" +
    "</div>" +
    "<div class=\"detail-payload-box\">" +
    "<div class=\"payload-header\"><span>请求报文 / 调试元数据</span></div>" +
    "<pre class=\"payload-code\"><code>" + escapeHtml(item.details || "无报文详情") + "</code></pre>" +
    "</div>";
}

async function refreshDashboard(): Promise<void> {
  if (!nativeAvailable) {
    usageOverview = previewUsageOverview(usageRange);
    if (requestLogs.length === 0) requestLogs = previewRequestLogs();
    renderDashboard();
    setStatus("浏览器预览已载入示例用量和调用记录。", "info");
    return;
  }
  await run("正在读取当前状态…", async () => {
    await refreshCodexAccountStatus();
    dashboard = await invoke<DashboardState>("inspect_state");
    await refreshProxyStatus();
        await refreshOutboundProxy(false);
    await fetchProxyRequestLogs();
    await fetchUsageOverview();
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
  renderDashboardPage();
  renderRetrySettings();
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

function renderRetrySettings(): void {
  const input = document.querySelector<HTMLInputElement>("#upstream-max-retries");
  const title = document.querySelector<HTMLElement>("#retry-summary-title");
  const copy = document.querySelector<HTMLElement>("#retry-summary-copy");
  const value = Number.isFinite(localProxy.upstreamMaxRetries)
    ? localProxy.upstreamMaxRetries
    : 8;
  if (input && document.activeElement !== input) input.value = String(value);
  if (title) title.textContent = `最多重试 ${value} 次`;
  if (copy) {
    const seconds = localProxy.upstreamRetryMaxElapsedSeconds || 90;
    copy.textContent = `单个请求的重试总等待时间上限为 ${seconds} 秒。`;
  }
}

function renderOfficialProfile(): void {
  const root = required<HTMLElement>("#official-profile");
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

  let badgeText = "未登录";
  let badgeClass = "badge badge-neutral";
  if (dashboard.officialProfileWarning) {
    badgeText = "需检查";
    badgeClass = "badge badge-warning";
  } else if (!codexAccountAvailable) {
    badgeText = "状态不可用";
    badgeClass = "badge badge-warning";
  } else if (accessTokenEnvironmentConflict) {
    badgeText = "环境冲突";
    badgeClass = "badge badge-warning";
  } else if (current) {
    badgeText = "当前";
    badgeClass = "badge badge-official";
  } else if (loggedIn) {
    badgeText = "已登录";
    badgeClass = "badge badge-neutral";
  } else if (codexAccount.authMode === "apiKey") {
    badgeText = "API Key";
    badgeClass = "badge badge-neutral";
  } else if (codexAccount.authMode !== "none") {
    badgeText = "其他认证";
  }

  const email = loggedIn
    ? (codexAccount.email ?? dashboard.officialProfile?.email ?? "Codex 未返回邮箱")
    : "";
  const plan = loggedIn
    ? formatPlanType(codexAccount.planType ?? dashboard.officialProfile?.planType)
    : "";
  const subtitle = loggedIn
    ? [email, plan].filter(Boolean).join(" · ")
    : officialAccountExplanation(current);
  const primaryDisabled = blocked || Boolean(dashboard.officialProfileWarning);
  const primaryAction = !loggedIn
    ? `<button class="button button-primary button-compact" data-official-action="login" type="button" ${primaryDisabled ? "disabled" : ""}>登录</button>`
    : !builtInRoute
      ? `<button class="button button-primary button-compact" data-official-action="activate" type="button" ${primaryDisabled ? "disabled" : ""}>使用</button>`
      : "";
  const overflowItems = loggedIn
    ? `
        <button class="row-menu-item" data-official-action="relogin" type="button" role="menuitem" ${blocked ? "disabled" : ""}>${builtInRoute ? "重新登录" : "登录其他账号"}</button>
        ${builtInRoute ? `<button class="row-menu-item danger-text" data-official-action="logout" type="button" role="menuitem" ${blocked ? "disabled" : ""}>退出账号</button>` : ""}
      `
    : "";
  const overflow = overflowItems
    ? `
      <div class="row-more">
        <button class="button button-icon" data-row-more type="button" aria-label="更多操作" aria-haspopup="true" aria-expanded="false">
          ${MORE_ICON}
        </button>
        <div class="row-menu" role="menu" hidden>
          ${overflowItems}
        </div>
      </div>
    `
    : "";
  const extra = codexAccount.codexAccessTokenEnvironmentPresent
    ? `
      <div class="list-row list-row-note">
        <p class="official-account-warning">检测到 CODEX_ACCESS_TOKEN。若 Codex 从同一环境启动，该外部访问令牌会优先于已保存的 ChatGPT 登录；清除后请完整退出并重新打开本软件和 Codex。</p>
      </div>
    `
    : "";

  root.innerHTML = `
    <div class="list-row provider-row ${current ? "provider-row-current" : ""}">
      <div class="provider-icon provider-icon-official" aria-hidden="true">O</div>
      <div class="list-copy">
        <strong id="official-title">OpenAI 官方账号</strong>
        <span>${escapeHtml(subtitle)}</span>
      </div>
      <span id="official-badge" class="${badgeClass}">${badgeText}</span>
      ${primaryAction}
      ${overflow}
    </div>
    ${extra}
  `;
  bindOverflowMenus(root);
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
          ? "请先处理"
          : switchMode === "directConfig"
          ? "写入"
          : localProxy.recoveryRequired
            ? "请先修复"
            : "使用";
      const initial =
        Array.from(profile.display_name.trim())[0]?.toLocaleUpperCase() ?? "A";
      return `
        <article class="list-row provider-row ${isCurrent ? "provider-row-current" : ""}">
          <div class="provider-icon provider-icon-api" aria-hidden="true">${escapeHtml(initial)}</div>
          <div class="list-copy">
            <strong>${escapeHtml(profile.display_name)}</strong>
            <span>${escapeHtml(readableEndpoint(profile.base_url))}</span>
          </div>
          ${isCurrent ? '<span class="badge">当前</span>' : ""}
          <select class="row-select" data-profile-model="${escapeHtml(profile.id)}" aria-label="选择模型">
            ${profile.models
              .map(
                (model) =>
                  `<option value="${escapeHtml(model.id)}" ${model.id === selected ? "selected" : ""}>${escapeHtml(model.display_name || model.id)}</option>`,
              )
              .join("")}
          </select>
          <button class="button button-primary button-compact" data-switch-profile="${escapeHtml(profile.id)}" type="button" ${manualRecoveryBlocked || (switchMode === "localProxy" && localProxy.recoveryRequired) ? "disabled" : ""}>${actionLabel}</button>
          <div class="row-more">
            <button class="button button-icon" data-row-more type="button" aria-label="更多操作" aria-haspopup="true" aria-expanded="false">
              ${MORE_ICON}
            </button>
            <div class="row-menu" role="menu" hidden>
              <button class="row-menu-item" data-edit-profile="${escapeHtml(profile.id)}" type="button" role="menuitem">编辑</button>
              <button class="row-menu-item danger-text" data-delete-profile="${escapeHtml(profile.id)}" type="button" role="menuitem">移除</button>
            </div>
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
      closeAllMenus();
      void openProfileEditor(button.dataset.editProfile ?? "");
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-delete-profile]").forEach((button) => {
    button.addEventListener("click", () => {
      closeAllMenus();
      deleteSavedProfile(button.dataset.deleteProfile ?? "");
    });
  });
  bindOverflowMenus(root);
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
  hiddenAliasCount = 0;
  setHiddenAliasHint(0);
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
  setContextWindowControl(DEFAULT_CONTEXT_WINDOW);
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
  hiddenAliasCount = 0;
  setHiddenAliasHint(0);
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
  setContextWindowControl(profileContextWindow(profile));
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
  hiddenAliasCount = 0;
  setHiddenAliasHint(0);
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
      hiddenAliasCount = 0;
      selectedModels.clear();
      required<HTMLElement>("#model-step").hidden = false;
      setEditorStep(2);
      renderModelChoices();
      setHiddenAliasHint(0);
      throw error;
    }
    if (!editingProfileId) {
      keyInput.value = "";
    }
    discoveryId = result.sessionId;
    input("#base-url").value = result.baseUrl;
    discoveredModels = result.models;
    hiddenAliasCount = result.hiddenAliasCount ?? 0;
    selectedModels.clear();
    required<HTMLElement>("#model-step").hidden = false;
    setEditorStep(2);
    renderModelChoices();
    setHiddenAliasHint(hiddenAliasCount);
    if (result.models.length === 0) {
      return "服务已连接，但没有返回模型。你可以手动填写模型 ID。";
    }
    return hiddenAliasCount > 0
      ? `已获取 ${result.models.length} 个模型，并隐藏 ${hiddenAliasCount} 个 x-ai/、grok/ 这类前缀别名。请勾选 grok-4.6 这种短 ID。`
      : `已获取 ${result.models.length} 个模型，请勾选需要保留的模型。`;
  });
}


function setHiddenAliasHint(count: number): void {
  const hint = document.querySelector<HTMLElement>("#model-alias-hint");
  if (!hint) return;
  if (count > 0) {
    hint.hidden = false;
    hint.textContent =
      "已自动隐藏 " +
      count +
      " 个带供应商前缀的别名（例如 x-ai/grok-4.6、grok/grok-imagine-video）。请勾选 grok-4.6 这种短 ID。";
  } else {
    hint.hidden = true;
  }
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

async function diagnoseToolChannel(showStatus = false): Promise<void> {
  try {
    toolChannel = await invoke<ToolChannelDiagnosis>("diagnose_codex_tool_channel");
    renderToolChannel();
    if (showStatus) {
      const issue = toolChannel.healthy
        ? "本机 Codex 工具通道未见已知沙箱故障。"
        : `检测到工具通道问题：${toolChannel.issues[0] || "未知"}`;
      setStatus(issue, toolChannel.healthy ? "info" : "error");
    }
  } catch (error) {
    toolChannel = null;
    renderToolChannel();
    if (showStatus) {
      setStatus(String(error) || "工具通道检测失败。", "error");
    }
  }
}

function renderToolChannel(): void {
  const title = document.querySelector<HTMLElement>("#tool-channel-title");
  const copy = document.querySelector<HTMLElement>("#tool-channel-copy");
  const list = document.querySelector<HTMLElement>("#tool-channel-issues");
  if (!title || !copy || !list) return;
  if (!toolChannel) {
    title.textContent = "尚未检测";
    copy.textContent = "打开高级设置后会自动检测本机 Codex 沙箱与权限模式。";
    list.hidden = true;
    list.innerHTML = "";
    return;
  }
  if (!toolChannel.platformSupported) {
    title.textContent = "当前系统无需此项";
    copy.textContent = toolChannel.recommendations[0] || "仅 Windows 需要沙箱修复。";
    list.hidden = true;
    list.innerHTML = "";
    return;
  }
  if (toolChannel.healthy) {
    title.textContent = "工具通道正常";
    copy.textContent = toolChannel.agentMode
      ? `当前 Agent 模式：${toolChannel.agentMode}`
      : "未发现 setup_error 或 Guardian 卡死。";
    list.hidden = true;
    list.innerHTML = "";
    return;
  }
  title.textContent = "工具通道异常";
  copy.textContent =
    toolChannel.recommendations[0] ||
    "建议一键修复：切到 Full access 并清理沙箱错误。";
  list.hidden = false;
  list.innerHTML = toolChannel.issues
    .slice(0, 6)
    .map((item) => `<li>${escapeHtml(item)}</li>`)
    .join("");
}

async function repairToolChannel(): Promise<void> {
  await run("正在修复 Codex 工具通道…", async () => {
    const report = await invoke<ToolChannelRepairReport>("repair_codex_tool_channel");
    toolChannel = report.diagnosis;
    renderToolChannel();
    const steps = report.steps.filter(Boolean).join("；");
    if (report.requiresCodexRestart) {
      try {
        await invoke("restart_codex");
      } catch {
        // User can restart manually if launch fails.
      }
    }
    if (!report.diagnosis.healthy && report.elevationNeeded && !report.elevationAttempted) {
      throw new Error(
        `${steps}。仍需管理员权限修复部分目录 ACL，请右键以管理员运行本应用后再点修复。`,
      );
    }
    if (!report.diagnosis.healthy) {
      return `${steps}。部分问题可能仍在，请完全退出 Codex 后重试工具。`;
    }
    return steps || "工具通道修复已完成。请完全退出并重开 Codex 后验证。";
  });
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

async function setUpstreamRetrySettings(): Promise<void> {
  const input = required<HTMLInputElement>("#upstream-max-retries");
  const value = Number(input.value);
  if (!Number.isInteger(value) || value < 0 || value > 20) {
    setStatus("重试次数必须是 0 到 20 之间的整数。", "error");
    return;
  }
  await run("正在应用上游重试设置…", async () => {
    localProxy = await invoke<LocalProxyStatus>("set_upstream_retry_settings", {
      maxRetries: value,
    });
    proxyApiAvailable = true;
    await refreshProxyStatus();
    renderDashboard();
    return `上游最大重试次数已设为 ${value} 次；单请求最长等待 90 秒。`;
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

function closeRestartNotice(): void {
  const dialog = document.querySelector<HTMLDialogElement>("#restart-notice");
  if (dialog?.open) dialog.close();
}

function closeUpdateNotice(): void {
  const dialog = document.querySelector<HTMLDialogElement>("#update-notice");
  if (dialog?.open) dialog.close();
}

function enableDialogActions(dialog: HTMLDialogElement): void {
  dialog.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
    button.disabled = false;
  });
}

function enableRestartNoticeActions(): void {
  const dialog = document.querySelector<HTMLDialogElement>("#restart-notice");
  if (dialog) enableDialogActions(dialog);
}

function enableUpdateNoticeActions(): void {
  const dialog = document.querySelector<HTMLDialogElement>("#update-notice");
  if (dialog) enableDialogActions(dialog);
}

function otherOpenDialog(dialog: HTMLDialogElement): HTMLDialogElement | undefined {
  return [...document.querySelectorAll("dialog")].find(
    (item): item is HTMLDialogElement =>
      item instanceof HTMLDialogElement && item.open && item !== dialog,
  );
}

function waitFrames(count = 2): Promise<void> {
  return new Promise((resolve) => {
    const step = (left: number) => {
      if (left <= 0) {
        resolve();
        return;
      }
      requestAnimationFrame(() => step(left - 1));
    };
    step(count);
  });
}

function maybeShowRestartNotice(): void {
  if (
    restartNoticeShown ||
    restartNoticePresenting ||
    !mainWindowShown ||
    !nativeAvailable ||
    !localProxy.running ||
    !localProxy.requiresCodexRestart
  ) {
    return;
  }

  const editor = document.querySelector<HTMLDialogElement>("#editor");
  if (editor?.open) {
    if (editor.dataset.restartOnClose !== "1") {
      editor.dataset.restartOnClose = "1";
      editor.addEventListener(
        "close",
        () => {
          delete editor.dataset.restartOnClose;
          maybeShowRestartNotice();
        },
        { once: true },
      );
    }
    return;
  }

  restartNoticePresenting = true;
  void presentRestartNotice();
}

async function presentRestartNotice(): Promise<void> {
  const dialog = required<HTMLDialogElement>("#restart-notice");
  enableRestartNoticeActions();

  try {
    // Programmatic <dialog showModal()> leaves WebView2's top layer eating
    // pointer events until Escape. These notices use a non-modal overlay instead.
    await waitFrames(2);
    const blocking = otherOpenDialog(dialog);
    if (blocking) {
      if (blocking.dataset.restartOnClose !== "1") {
        blocking.dataset.restartOnClose = "1";
        blocking.addEventListener(
          "close",
          () => {
            delete blocking.dataset.restartOnClose;
            maybeShowRestartNotice();
          },
          { once: true },
        );
      }
      return;
    }
    if (await presentNoticeDialog(dialog, "#restart-later")) {
      restartNoticeShown = true;
    }
  } finally {
    restartNoticePresenting = false;
  }
}

function formatContextWindow(tokens: number): string {
  if (tokens >= 1_000_000) {
    const millions = tokens / 1_000_000;
    return Number.isInteger(millions) ? `${millions}M` : `${millions.toFixed(1)}M`;
  }
  return `${Math.round(tokens / 1_000)}k`;
}

function contextWindowStepIndex(tokens: number): number {
  let closest = 0;
  let best = Number.POSITIVE_INFINITY;
  CONTEXT_WINDOW_STEPS.forEach((step, index) => {
    const distance = Math.abs(step - tokens);
    if (distance < best) {
      best = distance;
      closest = index;
    }
  });
  return closest;
}

function selectedContextWindow(): number {
  const index = Number(required<HTMLInputElement>("#context-window").value);
  return CONTEXT_WINDOW_STEPS[index] ?? DEFAULT_CONTEXT_WINDOW;
}

function profileContextWindow(profile: ProviderProfile): number {
  return profile.models.reduce(
    (max, model) => Math.max(max, model.context_window),
    profile.models[0]?.context_window ?? DEFAULT_CONTEXT_WINDOW,
  );
}

function setContextWindowControl(tokens: number): void {
  required<HTMLInputElement>("#context-window").value = String(contextWindowStepIndex(tokens));
  updateContextWindowControl();
}

function updateContextWindowControl(): void {
  const slider = required<HTMLInputElement>("#context-window");
  const tokens = selectedContextWindow();
  const label = formatContextWindow(tokens);
  const max = Number(slider.max) || CONTEXT_WINDOW_STEPS.length - 1;
  required<HTMLElement>("#context-window-value").textContent = label;
  slider.setAttribute("aria-valuenow", slider.value);
  slider.setAttribute("aria-valuetext", label);
  slider.style.setProperty(
    "--slider-progress",
    `${max === 0 ? 0 : (Number(slider.value) / max) * 100}%`,
  );
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
        context_window: selectedContextWindow(),
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

function startAutomaticUpdateChecks(): void {
  scheduleAutoUpdateCheck(AUTO_UPDATE_FIRST_DELAY_MS);
  window.addEventListener("focus", presentPendingUpdateIfNeeded);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) presentPendingUpdateIfNeeded();
  });
  if (!nativeAvailable) {
    mainWindowShown = true;
    return;
  }
  void listen("main-window-shown", () => {
    mainWindowShown = true;
    window.setTimeout(() => {
      void recoverNoticeDialogs();
    }, 200);
  }).catch(() => {
    mainWindowShown = true;
  });
}

async function recoverNoticeDialogs(): Promise<void> {
  const update = document.querySelector<HTMLDialogElement>("#update-notice");
  const restart = document.querySelector<HTMLDialogElement>("#restart-notice");
  if (update?.open) {
    update.close();
    await waitFrames(2);
  }
  if (restart?.open) {
    restart.close();
    await waitFrames(2);
  }
  presentPendingUpdateIfNeeded();
  maybeShowRestartNotice();
}

async function presentNoticeDialog(
  dialog: HTMLDialogElement,
  focusSelector: string,
): Promise<boolean> {
  enableDialogActions(dialog);
  await waitFrames(2);
  if (otherOpenDialog(dialog)) return false;
  if (dialog.open) {
    dialog.close();
    await waitFrames(2);
  }
  dialog.inert = false;
  dialog.removeAttribute("inert");
  dialog.show();
  dialog.inert = false;
  dialog.removeAttribute("inert");
  enableDialogActions(dialog);
  dialog.querySelector<HTMLButtonElement>(focusSelector)?.focus();
  return true;
}

function scheduleAutoUpdateCheck(delayMs: number): void {
  if (autoUpdateTimer !== null) window.clearTimeout(autoUpdateTimer);
  autoUpdateTimer = window.setTimeout(() => {
    autoUpdateTimer = null;
    void checkAppUpdate(false);
  }, delayMs);
}

function presentPendingUpdateIfNeeded(): void {
  if (!mainWindowShown || busy || restartNoticePresenting || updateNoticePresenting) return;
  const status = pendingUpdate;
  if (!status?.updateAvailable || status.skipped) return;
  const dialog = document.querySelector<HTMLDialogElement>("#update-notice");
  if (!dialog || dialog.open) return;
  if (document.hidden) return;
  if (otherOpenDialog(dialog)) return;
  showUpdateDialog(status);
}

async function checkAppUpdate(manual: boolean): Promise<void> {
  if (!nativeAvailable) {
    if (manual) {
      setStatus("请通过桌面应用检查更新。", "info");
    }
    return;
  }
  if (!manual && autoUpdateInFlight) return;
  if (!manual) autoUpdateInFlight = true;
  if (manual) {
    setBusy(true);
    setStatus("正在检查更新…", "working");
  }
  try {
    const status = await invoke<AppUpdateStatus>("check_app_update");
    autoUpdateRetryIndex = 0;
    required<HTMLElement>("#app-version").textContent = `Version ${status.currentVersion}`;
    if (!status.updateAvailable) {
      pendingUpdate = null;
      if (manual) {
        setStatus(`当前已是最新版本 ${status.currentVersion}`, "success");
      }
      scheduleAutoUpdateCheck(AUTO_UPDATE_INTERVAL_MS);
      return;
    }
    pendingUpdate = status;
    if (!manual && status.skipped) {
      scheduleAutoUpdateCheck(AUTO_UPDATE_INTERVAL_MS);
      return;
    }
    if (manual || (!busy && !document.hidden)) {
      showUpdateDialog(status);
    }
    if (manual) {
      setStatus(`发现新版本 ${status.latestVersion}`, "success");
    }
    scheduleAutoUpdateCheck(AUTO_UPDATE_INTERVAL_MS);
  } catch (error) {
    if (manual) setStatus(friendlyError(error), "error");
    else {
      const retryIndex = Math.min(
        autoUpdateRetryIndex,
        AUTO_UPDATE_RETRY_DELAYS_MS.length - 1,
      );
      autoUpdateRetryIndex += 1;
      scheduleAutoUpdateCheck(
        AUTO_UPDATE_RETRY_DELAYS_MS[retryIndex] ?? AUTO_UPDATE_INTERVAL_MS,
      );
    }
  } finally {
    if (manual) setBusy(false);
    if (!manual) autoUpdateInFlight = false;
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
  const blocking = otherOpenDialog(dialog);
  if (blocking) {
    if (blocking.dataset.updateOnClose !== "1") {
      blocking.dataset.updateOnClose = "1";
      blocking.addEventListener(
        "close",
        () => {
          delete blocking.dataset.updateOnClose;
          presentPendingUpdateIfNeeded();
        },
        { once: true },
      );
    }
    return;
  }
  if (updateNoticePresenting) return;
  updateNoticePresenting = true;
  void presentNoticeDialog(dialog, "#update-now").finally(() => {
    updateNoticePresenting = false;
  });
}

async function skipPendingUpdate(): Promise<void> {
  const status = pendingUpdate;
  pendingUpdate = status ? { ...status, skipped: true } : null;
  closeUpdateNotice();
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
  closeUpdateNotice();
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
  if (value) closeAllMenus();
  document.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
    // Keep dialog actions (restart/update/editor) out of the global busy lock.
    // Otherwise save-flow disables #restart-later/#restart-now before the
    // notice opens, then skips re-enabling them because the dialog is open.
    if (button.closest("dialog")) return;
    button.disabled = value;
  });
  document
    .querySelectorAll<HTMLInputElement | HTMLSelectElement>("input, select")
    .forEach((control) => {
      if (control.closest("dialog")) return;
      control.disabled = value;
    });
  if (!value) {
    enableRestartNoticeActions();
    enableUpdateNoticeActions();
    required<HTMLButtonElement>("#restore").disabled =
      !dashboard.latestBackup ||
      dashboard.recoveryWarnings > 0 ||
      localProxy.enabled ||
      localProxy.recoveryRequired;
    renderSwitchExperience();
    renderOfficialProfile();
    renderProfiles();
    updateSelectionSummary();
    presentPendingUpdateIfNeeded();
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
    message.includes("expected exactly one registered official OpenAI.Codex") ||
    message.includes("expected exactly one launchable application") ||
    message.includes("expected exactly one OpenAI.Codex install location") ||
    message.includes("no registered official OpenAI.Codex") ||
    message.includes("no launchable application in the OpenAI.Codex") ||
    message.includes("could not activate Codex") ||
    message.includes("Windows declined to activate Codex") ||
    message.includes("Windows could not activate Codex") ||
    message.includes("未找到可启动的官方 Codex")
  ) {
    return "未能重新打开官方 Codex。请确认 Microsoft Store 里的 ChatGPT/Codex 仍可打开，然后重试。";
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
  hiddenAliasCount = 0;
  selectedModels.clear();
  required<HTMLElement>("#model-step").hidden = true;
  setHiddenAliasHint(0);
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
