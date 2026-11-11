import { tx } from "../i18n";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  GlassRegion,
  GlassTint,
  WindowChrome,
  AnswerDecisionData,
  AnswerDecisionRequest,
  ApiEnvelope,
  ApiErrorPayload,
  ApproveCheckData,
  ApproveCheckRequest,
  ChangedEvent,
  CheckUpdatesData,
  ConfigureVaultData,
  ConfigureVaultRequest,
  EmptyRequest,
  EventMap,
  EventResultData,
  GetProjectData,
  GetProjectRequest,
  GetSetupLocationsData,
  LocalConfig,
  GetSnapshotRequest,
  HealthEvent,
  InstallLocalData,
  InstallLocalRequest,
  LinkRepoData,
  UnlinkRepoData,
  TestNoticeData,
  OpenReportData,
  OpenReportRequest,
  GetBindingsData,
  SetBindingRequest,
  SetBindingData,
  UnlinkRepoRequest,
  LinkRepoRequest,
  NotificationFocus,
  OpenPreviewData,
  OpenReleaseData,
  OpenPreviewRequest,
  PrepareAnswerData,
  PrepareAnswerRequest,
  ReadReportData,
  ReadReportRequest,
  ResolveRescueData,
  ResolveRescueRequest,
  ReviewDeliveryData,
  ReviewDeliveryRequest,
  RunCheckData,
  RunCheckRequest,
  SearchData,
  SearchRequest,
  SetPolicyRequest,
  SetPreferencesData,
  SetPreferencesRequest,
  SnoozeNeedData,
  SnoozeNeedRequest,
  DismissNeedRequest,
  DropStepsData,
  DropStepsRequest,
  SetRuleRequest,
  Snapshot,
  StorageUsage,
  UnblockEvent,
  UpdateFeatureRequest,
  UpdateEvent,
  UploadEvent,
  NavigateEvent,
  UpdateProjectRequest,
} from "./types";

export type Surface = "main" | "popover";

export class ApiError extends Error {
  readonly code: string;
  readonly field: string | undefined;
  readonly details: { field: string; message: string }[] | undefined;
  readonly hint: string;
  readonly retryable: boolean;

  constructor(payload: ApiErrorPayload) {
    super(payload.message);
    this.name = "ApiError";
    this.code = payload.code;
    this.field = payload.field;
    this.details = payload.details;
    this.hint = payload.hint;
    this.retryable = payload.retryable;
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError;
}

export function errorCode(error: unknown): string {
  return isApiError(error) ? error.code : "E_IO";
}

export interface Transport {
  invoke(command: string, request: unknown): Promise<unknown>;
  listen<K extends keyof EventMap>(
    event: K,
    handler: (payload: EventMap[K]) => void,
  ): Promise<UnlistenFn>;
}

export interface AgendarClient {
  /** Which transport backs this client. Fixture mode is development-only. */
  readonly mode: "tauri" | "fixture";
  getSnapshot(): Promise<Snapshot>;
  getSetupLocations(): Promise<GetSetupLocationsData>;
  getLocalConfig(): Promise<LocalConfig>;
  /** What each project holds and what a cleanup could free. Read-only. */
  getStorageUsage(): Promise<StorageUsage>;
  getProject(request: GetProjectRequest): Promise<GetProjectData>;
  search(request: SearchRequest): Promise<SearchData>;
  prepareAnswer(request: PrepareAnswerRequest): Promise<PrepareAnswerData>;
  answerDecision(request: AnswerDecisionRequest): Promise<AnswerDecisionData>;
  reviewDelivery(request: ReviewDeliveryRequest): Promise<ReviewDeliveryData>;
  resolveRescue(request: ResolveRescueRequest): Promise<ResolveRescueData>;
  updateProject(request: UpdateProjectRequest): Promise<EventResultData>;
  updateFeature(request: UpdateFeatureRequest): Promise<EventResultData>;
  /** The owner drops steps, all or none; a refused step is named by `field` `/nodes/<index>`. */
  dropSteps(request: DropStepsRequest): Promise<DropStepsData>;
  setPolicy(request: SetPolicyRequest): Promise<EventResultData>;
  setRule(request: SetRuleRequest): Promise<EventResultData>;
  readReport(request: ReadReportRequest): Promise<ReadReportData>;
  approveCheck(request: ApproveCheckRequest): Promise<ApproveCheckData>;
  runCheck(request: RunCheckRequest): Promise<RunCheckData>;
  setPreferences(request: SetPreferencesRequest): Promise<SetPreferencesData>;
  snoozeNeed(request: SnoozeNeedRequest): Promise<SnoozeNeedData>;
  dismissNeed(request: DismissNeedRequest): Promise<SetPreferencesData>;
  configureVault(request: ConfigureVaultRequest): Promise<ConfigureVaultData>;
  installLocal(request: InstallLocalRequest): Promise<InstallLocalData>;
  linkRepo(request: LinkRepoRequest): Promise<LinkRepoData>;
  unlinkRepo(request: UnlinkRepoRequest): Promise<UnlinkRepoData>;
  testNotice(): Promise<TestNoticeData>;
  openReport(request: OpenReportRequest): Promise<OpenReportData>;
  getBindings(): Promise<GetBindingsData>;
  setBinding(request: SetBindingRequest): Promise<SetBindingData>;
  /** Bring the main window forward, on one Needs-you item or Settings › About when given. */
  showMain(target?: { project: string; item_id: string } | { view: "about" }): Promise<void>;
  hidePopover(): Promise<void>;
  windowChrome(): Promise<WindowChrome>;
  setGlassRegions(regions: GlassRegion[], tint: GlassTint | null): Promise<void>;
  openPreview(request: OpenPreviewRequest): Promise<OpenPreviewData>;
  /** Ask GitHub once for the latest release (refused while checks are off). */
  checkUpdates(): Promise<CheckUpdatesData>;
  /** Open the latest release page in the browser. */
  openRelease(): Promise<OpenReleaseData>;
  onChanged(handler: (event: ChangedEvent) => void): Promise<UnlistenFn>;
  onHealth(handler: (event: HealthEvent) => void): Promise<UnlistenFn>;
  onUnblock(handler: (event: UnblockEvent) => void): Promise<UnlistenFn>;
  onNotificationFocus(handler: (event: NotificationFocus) => void): Promise<UnlistenFn>;
  /** An update check started or finished. */
  onUpdate(handler: (event: UpdateEvent) => void): Promise<UnlistenFn>;
  /** This computer's writes started or stopped waiting for their upload. */
  onUpload(handler: (event: UploadEvent) => void): Promise<UnlistenFn>;
  onNavigate(handler: (event: NavigateEvent) => void): Promise<UnlistenFn>;
}

export function createTauriTransport(): Transport {
  return {
    async invoke(command, request) {
      return invoke<unknown>(command, { request });
    },
    async listen(event, handler) {
      return listen<EventMap[typeof event]>(event, (message) => handler(message.payload));
    },
  };
}

export function createClient(transport: Transport, mode: "tauri" | "fixture" = "tauri"): AgendarClient {
  async function call<T>(command: string, request: unknown): Promise<T> {
    const envelope = (await transport.invoke(command, request)) as ApiEnvelope<T>;
    if (!envelope || envelope.v !== 1) {
      throw new ApiError({
        code: "E_IO",
        message: tx("api.unreadable"),
        hint: tx("rs.retry"),
        retryable: true,
      });
    }
    if (!envelope.ok) throw new ApiError(envelope.error);
    return envelope.data;
  }

  return {
    mode,
    getSnapshot: () => call<Snapshot>("get_snapshot", {} satisfies GetSnapshotRequest),
    getSetupLocations: () =>
      call<GetSetupLocationsData>("get_setup_locations", {} satisfies EmptyRequest),
    getLocalConfig: () => call<LocalConfig>("get_local_config", {} satisfies EmptyRequest),
    getStorageUsage: () => call<StorageUsage>("get_storage_usage", {} satisfies EmptyRequest),
    getProject: (request) => call<GetProjectData>("get_project", request),
    search: (request) => call<SearchData>("search", request),
    prepareAnswer: (request) => call<PrepareAnswerData>("prepare_answer", request),
    answerDecision: (request) => call<AnswerDecisionData>("answer_decision", request),
    reviewDelivery: (request) => call<ReviewDeliveryData>("review_delivery", request),
    resolveRescue: (request) => call<ResolveRescueData>("resolve_rescue", request),
    updateProject: (request) => call<EventResultData>("update_project", request),
    updateFeature: (request) => call<EventResultData>("update_feature", request),
    dropSteps: (request) => call<DropStepsData>("drop_steps", request),
    setPolicy: (request) => call<EventResultData>("set_policy", request),
    setRule: (request) => call<EventResultData>("set_rule", request),
    readReport: (request) => call<ReadReportData>("read_report", request),
    approveCheck: (request) => call<ApproveCheckData>("approve_check", request),
    runCheck: (request) => call<RunCheckData>("run_check", request),
    setPreferences: (request) => call<SetPreferencesData>("set_preferences", request),
    snoozeNeed: (request) => call<SnoozeNeedData>("snooze_need", request),
    dismissNeed: (request) => call<SetPreferencesData>("dismiss_need", request),
    configureVault: (request) => call<ConfigureVaultData>("configure_vault", request),
    installLocal: (request) => call<InstallLocalData>("install_local", request),
    linkRepo: (request) => call<LinkRepoData>("link_repo", request),
    unlinkRepo: (request) => call<UnlinkRepoData>("unlink_repo", request),
    testNotice: () => call<TestNoticeData>("test_notice", {} satisfies EmptyRequest),
    openReport: (request) => call<OpenReportData>("open_report", request),
    getBindings: () => call<GetBindingsData>("get_bindings", {} satisfies EmptyRequest),
    setBinding: (request) => call<SetBindingData>("set_binding", request),
    showMain: async (target) => {
      await call<Record<string, never>>("show_main", target ?? ({} satisfies EmptyRequest));
    },
    hidePopover: async () => {
      await call<Record<string, never>>("hide_popover", {} satisfies EmptyRequest);
    },
    windowChrome: () => call<WindowChrome>("window_chrome", {} satisfies EmptyRequest),
    setGlassRegions: async (regions, tint) => {
      await call<Record<string, never>>("set_glass_regions", tint ? { regions, tint } : { regions });
    },
    openPreview: (request) => call<OpenPreviewData>("open_preview", request),
    checkUpdates: () => call<CheckUpdatesData>("check_updates", {} satisfies EmptyRequest),
    openRelease: () => call<OpenReleaseData>("open_release", {} satisfies EmptyRequest),
    onChanged: (handler) => transport.listen("agendar://changed", handler),
    onHealth: (handler) => transport.listen("agendar://health", handler),
    onUnblock: (handler) => transport.listen("agendar://unblock", handler),
    onNotificationFocus: (handler) =>
      transport.listen("agendar://notification-focus", handler),
    onUpdate: (handler) => transport.listen("agendar://update", handler),
    onUpload: (handler) => transport.listen("agendar://upload", handler),
    onNavigate: (handler) => transport.listen("agendar://navigate", handler),
  };
}

function hasTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  const scope = window as unknown as Record<string, unknown>;
  return "__TAURI_INTERNALS__" in scope || "__TAURI__" in scope;
}

/**
 * Resolve the transport for this build.
 *
 * Fixture mode is compiled out of production: the dev flag is unset in every
 * build, so the dynamic import below is removed. Inside the native shell the
 * Tauri runtime marker wins even in dev, so real state can never be replaced by
 * labelled fixtures by accident.
 */
export async function resolveClient(): Promise<AgendarClient> {
  const dataset = import.meta.env.VITE_AGENDAR_FIXTURES;
  const fixturesRequested = import.meta.env.DEV && (dataset === "1" || dataset === "showcase");
  if (fixturesRequested && !hasTauriRuntime()) {
    const { createFixtureTransport } = await import("../dev/fixtureTransport");
    // The showcase renders the README screenshots: no development banner.
    return createClient(
      createFixtureTransport(dataset === "showcase" ? "showcase" : "test"),
      dataset === "showcase" ? "tauri" : "fixture",
    );
  }
  return createClient(createTauriTransport(), "tauri");
}

export function surfaceFromLocation(search: string): Surface {
  return new URLSearchParams(search).get("surface") === "popover" ? "popover" : "main";
}
