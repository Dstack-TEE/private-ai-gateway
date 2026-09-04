using System.Text.Json.Serialization;

namespace PrivateAIGateway.Windows;

public sealed record ProfileAuth(
    [property: JsonPropertyName("kind")] string Kind,
    [property: JsonPropertyName("accountId")] string? AccountId,
    [property: JsonPropertyName("accountName")] string? AccountName);

public sealed record ConfidentialProfile(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("provider")] string Provider,
    [property: JsonPropertyName("remoteUrl")] string RemoteUrl,
    [property: JsonPropertyName("auth")] ProfileAuth Auth,
    [property: JsonPropertyName("verifiedAt")] ulong? VerifiedAt);

public sealed record ConfidentialProfileInput(string Id, string Name, string Provider, string RemoteUrl);
public sealed record StartGatewayConfig(string RemoteUrl, bool RequireProductionOs);
public sealed record LocalApiConfig(string ListenAddress, bool AllowNetworkAccess, ushort Port, string? ClientHost);
public sealed record SourceProvenance(string? RepoUrl, string? RepoCommit, string? ImageDigest);
public sealed record GatewayIdentity(string TeeType, string TrustLevel, string KeysetDigest, ulong KeysetNotAfter, string? TlsSpki, SourceProvenance Source, string Serving, string[] SupportedE2eeVersions);
public sealed record VerificationCheck(string Id, string Section, string Title, string Status, string Detail);
public sealed record ModelSummary(string Id, string Name, ulong? ContextLength, ulong? MaxOutputLength, bool? IsTee, double? InputPricePerMillion, double? OutputPricePerMillion, double? CacheReadPricePerMillion, double? CacheWritePricePerMillion, string[] InputModalities, string[] OutputModalities, string[] Capabilities, string? Description);
public sealed record CatalogSummary(string Revision, ulong FetchedAt, ModelSummary[] Models, string[] Removed);
public sealed record RequestActivity(string Id, string SessionId, string Method, string Path, string? Model, ushort Status, bool Streamed, string? ReceiptId, bool? Verified, string Detail, ulong At, string? Agent, bool? LocallyConstrained, bool? Rewritten, bool LeftDevice, ulong? InputTokens, ulong? OutputTokens, ulong? CacheReadTokens, ulong? CacheWriteTokens, double? CostUsd);
public sealed record UsageSummary(ulong Requests, ulong InputTokens, ulong OutputTokens, ulong CacheReadTokens, ulong CacheWriteTokens, double CostUsd, ulong Protected, ulong BlockedLocally, ulong FailedProof)
{
    public static UsageSummary Empty { get; } = new(0, 0, 0, 0, 0, 0, 0, 0, 0);
}
public sealed record UsagePoint(string Day, ulong Requests, ulong InputTokens, ulong OutputTokens, ulong Tokens, double CostUsd);
public sealed record UsagePage(RequestActivity[] Items, string? NextCursor, UsageSummary Summary, UsagePoint[] Series, string[] Agents, string[] Models)
{
    public static UsagePage Empty { get; } = new([], null, UsageSummary.Empty, [], [], []);
}
public sealed record UsageQuery(string? Agent, string? Model, string? SessionId, ulong? Since, ulong? Until, string? Cursor, int? Limit);
public sealed record AgentStatus(string Id, string Name, string ConfigPath, bool Installed, bool Connected, bool Recorded, bool Authorized, string? Attention, string? Error);
public sealed record ConfigChange(string Key, string? Before, string? After, bool Sensitive);
public sealed record ConnectOptions(string? DefaultModel);
public sealed record AgentPreview(AgentStatus Agent, bool Connect, ConfigChange[] Changes, string Note, string Revision);

public sealed record GatewayState(string Status, bool ConfigurationVerification, string? Progress, string? RemoteUrl, string? ProxyUrl, string? EndpointError, GatewayIdentity? Identity, VerificationCheck[] Checks, RequestActivity[] Activity, string? SessionId, UsageSummary SessionUsage, ulong UsageRevision, string? Error, StartGatewayConfig Config, ConfidentialProfile[] Profiles, string ActiveProfileId, LocalApiConfig LocalApi, bool ApiKeySaved, CatalogSummary? Catalog)
{
    public static GatewayState Empty { get; } = new(
        "stopped", false, null, null, null, null, null, [], [], null, UsageSummary.Empty,
        0, null, new("", true), [], "", new("127.0.0.1", false, 4180, null), false, null);
}

public sealed record StartParams(StartGatewayConfig Config);
public sealed record VerifyParams(ConfidentialProfileInput Profile, bool RequireProductionOs, string? Key);
public sealed record ProfileParams(string ProfileId);
public sealed record UsageParams(UsageQuery Query);
public sealed record ExportUsageParams(UsageQuery Query, string Path);
public sealed record AgentParams(string AgentId, bool Connect, ConnectOptions Options);
public sealed record ApplyAgentParams(string AgentId, bool Connect, string Revision, ConnectOptions Options);
public sealed record LocalApiParams(LocalApiConfig Config);
