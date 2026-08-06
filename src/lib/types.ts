export type RuntimeState =
  | "stopped"
  | "starting"
  | "running"
  | "recovering"
  | "stopping"
  | "error";

export const DEFAULT_SERVICE_PORT = 28766;
export const DEFAULT_ACTIONS_PORT = 8787;

export interface TunnelConfig {
  type: string;
  public_url: string;
  frp_server: string;
  frp_subdomain: string;
  frp_profile_id?: string;
  frp_server_port?: number;
  frp_proxy_type?: string;
  frp_cert_path?: string;
  frp_key_path?: string;
  cloudflare_mode: string;
  use_proxy?: boolean;
}

export interface SkillFileSummary {
  path: string;
  kind: "resource" | "script";
  sizeBytes: number;
  mimeType: string;
  readable: boolean;
  digest: string;
}

export interface SkillToolResolution {
  declared: string;
  status: "resolved" | "missing" | "ambiguous";
  resolved?: string | null;
  candidates: string[];
}

export interface SkillSummary {
  name: string;
  description: string;
  license?: string | null;
  compatibility?: string | null;
  metadata: Record<string, string>;
  allowedTools: string[];
  resolvedTools: string[];
  missingTools: string[];
  ambiguousTools: string[];
  toolResolution: SkillToolResolution[];
  toolDependenciesEvaluated: boolean;
  toolCompatible: boolean;
  toolEnforcementMode: string;
  toolGrantsPermissions: boolean;
  source: "workspace" | "home" | "external";
  sourceId: string;
  relativePath: string;
  uri: string;
  digest: string;
  resources: SkillFileSummary[];
  scripts: SkillFileSummary[];
  scriptExecutionEnabled: boolean;
  scriptExecutionPolicy: string;
  resourceTruncated: boolean;
  warnings: string[];
}

export interface SkillInspection {
  enabled: boolean;
  roots: string[];
  skills: SkillSummary[];
  warnings: string[];
  truncated: boolean;
  scriptExecutionEnabled: boolean;
  scriptExecutionPolicy: string;
  snapshotMode: string;
  catalogDigest: string;
}

export interface AuthConfig {
  type: string;
  oauth_client_id: string;
  oauth_redirect_uris?: string;
  oauth_redirect_hosts?: string;
  use_shared_secrets?: boolean;
}

export interface RuntimeConfig {
  local_port: number;
  tool_profile: string;
  permission_mode: string;
  runtime_command?: string;
  mcp_config?: string;
  allowed_commands?: string;
  workspace_local_entries?: boolean;
  workspace_script_extensions?: string;
  skill_service_enabled?: boolean;
  skill_roots?: string;
  strict_workspace_reads?: boolean;
  external_paid_commands_enabled?: boolean;
  external_paid_max_runs_per_day?: number;
  external_paid_max_duration_seconds?: number;
}

export interface ActionsConfig {
  public_url: string;
  tunnel_type: string;
  frp_server: string;
  frp_subdomain: string;
  frp_profile_id?: string;
  frp_server_port?: number;
  frp_proxy_type?: string;
  frp_cert_path?: string;
  frp_key_path?: string;
  cloudflare_mode: string;
  use_proxy?: boolean;
  local_port: number;
  permission_mode: string;
  runtime_command?: string;
  auth_type: string;
  oauth_client_id?: string;
  oauth_redirect_uris?: string;
  oauth_redirect_hosts?: string;
  oauth_scopes?: string;
  allowed_commands?: string;
  max_patch_bytes?: number;
  use_shared_secrets?: boolean;
}

export interface WorkspaceProfile {
  id: string;
  name: string;
  path: string;
  tunnel: TunnelConfig;
  auth: AuthConfig;
  runtime: RuntimeConfig;
  actions?: ActionsConfig;
}

export interface RuntimeStatus {
  state: RuntimeState;
  pid: number | null;
  localMessage: string;
  publicMessage: string;
  localEndpoint: string;
  publicEndpoint: string;
  recovery: RuntimeRecovery;
  activity?: McpActivity | null;
}

export type McpActivityState =
  | "unknown"
  | "idle"
  | "recent"
  | "active"
  | "suspected_stalled";

export interface McpActivity {
  state: McpActivityState;
  message: string;
  inFlightRequests: number;
  oldestInFlightMs: number | null;
  lastActivityAt: string | null;
  lastActivityAgeMs: number | null;
  lastCompletedAt: string | null;
  currentMethod: string;
  currentTool: string;
  completedRequests: number;
  recentWindowMs: number;
  suspectedStallAfterMs: number;
}

export interface RuntimeRecovery {
  enabled: boolean;
  attempt: number;
  maxAttempts: number;
  retryInMs: number | null;
  recoveredCount: number;
  lastError: string;
}

export function actionsConfig(profile: WorkspaceProfile): ActionsConfig {
  return {
    public_url: "",
    tunnel_type: "cloudflare",
    frp_server: "",
    frp_subdomain: "",
    cloudflare_mode: "named",
    local_port: DEFAULT_ACTIONS_PORT,
    permission_mode: "trusted",
    auth_type: "api_key",
    allowed_commands:
      "pytest,python,python3,npm,npx,node,pnpm,yarn,make,mvn,mvnw,gradle,gradlew,cargo,go,ruff,mypy,eslint,tsc",
    max_patch_bytes: 200_000,
    ...profile.actions,
  };
}

export function mcpLocalEndpoint(port: number): string {
  return `http://127.0.0.1:${port}/mcp`;
}

export function actionsLocalEndpoint(port: number): string {
  return `http://127.0.0.1:${port}`;
}

export interface ActionsAuthDraft {
  authType: string;
  oauthClientId: string;
  oauthRedirectUris: string;
  oauthRedirectHosts: string;
  oauthScopes: string;
  useSharedSecrets?: boolean;
}

export interface FrpProfileSummary {
  id: string;
  name: string;
  server: string;
  serverPort: number;
}

export function frpPublicUrl(
  tunnelType: string,
  frpSubdomain: string,
  frpServer: string,
  frpProfileId: string | undefined,
  profiles: FrpProfileSummary[],
  publicUrl = "",
): string {
  if (tunnelType !== "frp" || !frpSubdomain) {
    return publicUrl.replace(/\/$/, "");
  }
  const explicit = publicUrl.replace(/\/$/, "");
  if (explicit) return explicit;
  const server =
    profiles.find((profile) => profile.id === frpProfileId)?.server ?? frpServer;
  if (!server) return publicUrl.replace(/\/$/, "");
  return `https://${frpSubdomain}.${server}`;
}

export function actionsPublicBaseUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const actions = actionsConfig(profile);
  const publicUrl = frpPublicUrl(
    actions.tunnel_type,
    actions.frp_subdomain,
    actions.frp_server,
    actions.frp_profile_id,
    frpProfiles,
    actions.public_url,
  );
  if (publicUrl) return publicUrl;
  return actionsLocalEndpoint(actions.local_port);
}

export function actionsOpenApiUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/openapi.json` : "";
}

export function actionsPrivacyUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/privacy` : "";
}

export function actionsOAuthAuthorizeUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/oauth/authorize` : "";
}

export function actionsOAuthTokenUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/oauth/token` : "";
}
